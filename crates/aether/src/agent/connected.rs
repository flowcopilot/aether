//! The connected-agent adapter: an agent that runs on its own answers the
//! tasks that the node sends to its agent socket.
//!
//! The Hermes and `OpenClaw` plugins are such agents. The rules, the constants
//! and the transcript that the tests replay are in `protocol/agent-socket/`.

use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Read},
    net::Shutdown,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use super::{EarlyCancels, write_json_line};
use crate::execution::{
    Agent, ExecutionError, ExecutionEvent, TurnContext, agent_instructions,
};

const PROTOCOL: u64 = 1;

const MAX_LINE: u64 = 4 * 1024 * 1024;

const ACCEPT_BACKOFF: Duration = Duration::from_millis(250);

#[derive(Clone)]
pub struct ConnectedAgent {
    shared: Arc<Shared>,
}

struct Shared {
    connection: Mutex<Option<Connection>>,
    generation: AtomicU64,
    cancels: Mutex<EarlyCancels>,
}

type Writer = Arc<Mutex<UnixStream>>;

type Pending = Arc<Mutex<Turns>>;

/// The turns that are open on one connection.
///
/// A turn holds this lock from the insert of its task until the `task`
/// message is written. A cancel therefore finds a task here only after the
/// agent got it, and `serve` closes the turns of a connection only between
/// two such writes.
#[derive(Default)]
struct Turns {
    open: BTreeMap<String, mpsc::Sender<Reply>>,
    /// The connection was replaced or ended. No turn starts on it.
    closed: bool,
}

impl Turns {
    /// Closes the connection for turns and returns the open ones.
    fn close(&mut self) -> BTreeMap<String, mpsc::Sender<Reply>> {
        self.closed = true;
        std::mem::take(&mut self.open)
    }
}

struct Connection {
    generation: u64,
    stream: UnixStream,
    writer: Writer,
    pending: Pending,
}

#[derive(Debug)]
enum Reply {
    Progress,
    Result(String),
    Failed,
    Cancelled,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum NodeMessage<'a> {
    Task { id: &'a str, instructions: String, text: &'a str },
    Cancel { id: &'a str },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AgentMessage {
    Ready { protocol: u64 },
    Progress { id: String },
    Result { id: String, text: String },
    Failed { id: String },
    Cancelled { id: String },
}

impl ConnectedAgent {
    pub fn listen(path: &Path) -> io::Result<Self> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_socket() => {
                std::fs::remove_file(path)?;
            }
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "the agent socket path is not a socket",
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        let agent = Self {
            shared: Arc::new(Shared {
                connection: Mutex::new(None),
                generation: AtomicU64::new(0),
                cancels: Mutex::new(EarlyCancels::default()),
            }),
        };
        let shared = Arc::clone(&agent.shared);
        thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        let shared = Arc::clone(&shared);
                        thread::spawn(move || shared.serve(stream));
                    }
                    Err(error) => {
                        eprintln!("aether: agent socket accept failed: {error}");
                        if error.kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        if error.kind() == io::ErrorKind::WouldBlock {
                            thread::sleep(ACCEPT_BACKOFF);
                            continue;
                        }
                        break;
                    }
                }
            }
        });
        Ok(agent)
    }

    fn connection(&self) -> Result<(Writer, Pending), ExecutionError> {
        self.shared
            .connection
            .lock()
            .map_err(|_| ExecutionError::Unavailable)?
            .as_ref()
            .map(|connection| {
                (Arc::clone(&connection.writer), Arc::clone(&connection.pending))
            })
            .ok_or(ExecutionError::Unavailable)
    }

    #[cfg(test)]
    fn generation(&self) -> Option<u64> {
        self.shared
            .connection
            .lock()
            .ok()?
            .as_ref()
            .map(|connection| connection.generation)
    }
}

impl Shared {
    fn serve(&self, stream: UnixStream) {
        let (Ok(writer), Ok(control)) = (stream.try_clone(), stream.try_clone()) else {
            return;
        };
        let mut reader = BufReader::new(stream);
        match read_message(&mut reader) {
            Some(AgentMessage::Ready { protocol: PROTOCOL }) => {}
            _ => return,
        }
        let generation =
            self.generation.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let pending = Arc::new(Mutex::new(Turns::default()));
        let Ok(mut connection) = self.connection.lock() else { return };
        // The replaced connection is closed for turns before the new one
        // takes its place. A turn that took it writes no task to it after
        // this, and no answer of its agent is read into a turn.
        let cancelled = connection
            .as_ref()
            .and_then(|replaced| {
                replaced.pending.lock().ok().map(|mut turns| turns.close())
            })
            .unwrap_or_default();
        let replaced = connection.replace(Connection {
            generation,
            stream: control,
            writer: Arc::new(Mutex::new(writer)),
            pending: Arc::clone(&pending),
        });
        drop(connection);
        if let Some(replaced) = replaced {
            for (task_id, sender) in cancelled {
                let _ = write_json_line(
                    &replaced.writer,
                    &NodeMessage::Cancel { id: task_id.as_str() },
                );
                let _ = sender.send(Reply::Cancelled);
            }
            let _ = replaced.stream.shutdown(Shutdown::Both);
        }
        while let Some(message) = read_message(&mut reader) {
            let (id, reply) = match message {
                AgentMessage::Ready { .. } => continue,
                AgentMessage::Progress { id } => (id, Reply::Progress),
                AgentMessage::Result { id, text } => (id, Reply::Result(text)),
                AgentMessage::Failed { id } => (id, Reply::Failed),
                AgentMessage::Cancelled { id } => (id, Reply::Cancelled),
            };
            if let Ok(turns) = pending.lock()
                && let Some(sender) = turns.open.get(&id)
            {
                let _ = sender.send(reply);
            }
        }
        // Under the `connection` mutex, as in the replace path: a turn
        // that took this connection but is not open yet does not start.
        // Each sender is dropped, so a turn that waits fails.
        let connection = self.connection.lock();
        if let Ok(mut turns) = pending.lock() {
            drop(turns.close());
        }
        if let Ok(mut connection) = connection
            && connection
                .as_ref()
                .is_some_and(|current| current.generation == generation)
        {
            *connection = None;
        }
    }
}

fn read_message(reader: &mut impl BufRead) -> Option<AgentMessage> {
    loop {
        let mut line = Vec::new();
        let read = reader.by_ref().take(MAX_LINE).read_until(b'\n', &mut line).ok()?;
        if read == 0 || line.last() != Some(&b'\n') {
            return None;
        }
        if let Ok(message) = serde_json::from_slice(&line) {
            return Some(message);
        }
    }
}

impl ConnectedAgent {
    fn run_turn(
        &self,
        ctx: &TurnContext,
        events: &mut dyn FnMut(ExecutionEvent),
    ) -> Result<String, ExecutionError> {
        let (writer, pending, receiver, sent) = {
            // Held until the turn is open, so that a cancel either finds
            // it open or is kept for this check.
            let mut cancels =
                self.shared.cancels.lock().map_err(|_| ExecutionError::Unavailable)?;
            if cancels.take(&ctx.task_id) {
                return Err(ExecutionError::Cancelled);
            }
            let (writer, pending) = self.connection()?;
            let (sender, receiver) = mpsc::channel();
            let sent = {
                let mut turns =
                    pending.lock().map_err(|_| ExecutionError::Unavailable)?;
                if turns.closed {
                    return Err(ExecutionError::Unavailable);
                }
                if turns.open.contains_key(&ctx.task_id) {
                    return Err(ExecutionError::Protocol);
                }
                turns.open.insert(ctx.task_id.clone(), sender);
                drop(cancels);
                // Written before `turns` is unlocked: a cancel waits for
                // this lock, so the agent gets the task first.
                let sent = write_json_line(
                    &writer,
                    &NodeMessage::Task {
                        id: &ctx.task_id,
                        instructions: agent_instructions(&ctx.skill),
                        text: &ctx.text,
                    },
                );
                drop(turns);
                sent
            };
            (writer, pending, receiver, sent)
        };
        let result = (|| {
            sent?;
            loop {
                let remaining = ctx
                    .deadline
                    .checked_duration_since(Instant::now())
                    .ok_or(ExecutionError::Timeout)?;
                match receiver.recv_timeout(remaining) {
                    Ok(Reply::Progress) => events(ExecutionEvent::Progress),
                    Ok(Reply::Result(text)) => {
                        events(ExecutionEvent::Completed);
                        return Ok(text);
                    }
                    Ok(Reply::Failed) => return Err(ExecutionError::Unavailable),
                    Ok(Reply::Cancelled) => return Err(ExecutionError::Cancelled),
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        return Err(ExecutionError::Timeout);
                    }
                    Err(mpsc::RecvTimeoutError::Disconnected) => {
                        return Err(ExecutionError::Unavailable);
                    }
                }
            }
        })();
        if let Ok(mut turns) = pending.lock() {
            turns.open.remove(&ctx.task_id);
        }
        if result == Err(ExecutionError::Timeout) {
            write_json_line(&writer, &NodeMessage::Cancel { id: &ctx.task_id })?;
        }
        result
    }
}

impl Agent for ConnectedAgent {
    fn start_turn_with_events(
        &mut self,
        ctx: &TurnContext,
        events: &mut dyn FnMut(ExecutionEvent),
    ) -> Result<String, ExecutionError> {
        let result = self.run_turn(ctx, events);
        if let Ok(mut cancels) = self.shared.cancels.lock() {
            cancels.end(&ctx.task_id);
        }
        result
    }

    fn cancel(&self, task_id: &str) {
        let Ok(mut cancels) = self.shared.cancels.lock() else { return };
        if let Ok((writer, pending)) = self.connection()
            && pending.lock().is_ok_and(|turns| turns.open.contains_key(task_id))
        {
            drop(cancels);
            let _ = write_json_line(&writer, &NodeMessage::Cancel { id: task_id });
            return;
        }
        cancels.cancel(task_id);
    }

    fn forget(&self, task_id: &str) {
        if let Ok(mut cancels) = self.shared.cancels.lock() {
            cancels.forget(task_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentMessage, ConnectedAgent, MAX_LINE, NodeMessage, PROTOCOL};
    use crate::execution::{
        Agent, ExecutionError, GENERAL_SKILL, TurnContext, agent_instructions,
    };
    use serde_json::{Value, json};
    use std::{
        collections::BTreeMap,
        io::{BufRead, BufReader, Read, Write},
        os::unix::{fs::PermissionsExt, net::UnixStream},
        path::Path,
        thread::JoinHandle,
        time::{Duration, Instant},
    };

    const CONTRACT: &str =
        include_str!("../../../../protocol/agent-socket/contract.json");
    const TRANSCRIPT: &str =
        include_str!("../../../../protocol/agent-socket/transcript.json");

    fn json_file(text: &str) -> Value {
        serde_json::from_str(text).expect("the protocol file is JSON")
    }

    struct Peer {
        stream: UnixStream,
        lines: BufReader<UnixStream>,
    }

    impl Peer {
        fn attach(agent: &ConnectedAgent, path: &Path, ready: &Value) -> Self {
            let before = agent.generation();
            let mut peer = Self::connect(path);
            peer.write(ready);
            let deadline = Instant::now()
                .checked_add(Duration::from_secs(5))
                .expect("deadline must fit");
            while agent.generation().is_none() || agent.generation() == before {
                assert!(Instant::now() < deadline, "the agent must attach");
                std::thread::sleep(Duration::from_millis(1));
            }
            peer
        }

        fn connect(path: &Path) -> Self {
            let stream = UnixStream::connect(path).expect("the agent must connect");
            let lines = BufReader::new(stream.try_clone().expect("stream must clone"));
            Self { stream, lines }
        }

        fn write(&mut self, message: &Value) {
            self.write_json_line(
                &serde_json::to_string(message).expect("message encodes"),
            );
        }

        fn write_json_line(&mut self, line: &str) {
            self.stream
                .write_all(format!("{line}\n").as_bytes())
                .expect("the line must send");
        }

        fn read(&mut self) -> Value {
            let mut line = String::new();
            self.lines.read_line(&mut line).expect("the node must send a line");
            serde_json::from_str(&line).expect("the node sends JSON")
        }

        fn closed(&mut self) -> bool {
            let mut rest = Vec::new();
            match self.lines.read_to_end(&mut rest) {
                Ok(_) => rest.is_empty(),
                Err(_) => true,
            }
        }
    }

    fn turn_ctx(id: &str, text: &str, deadline: Instant) -> TurnContext {
        TurnContext {
            task_id: id.to_owned(),
            skill: GENERAL_SKILL.to_owned(),
            text: text.to_owned(),
            workspace: std::env::temp_dir(),
            deadline,
        }
    }

    fn noise(bytes: u64) -> Vec<u8> {
        let mut line = br#"{"type":"noise","pad":""#.to_vec();
        let filler = usize::try_from(bytes)
            .expect("size fits")
            .saturating_sub(line.len())
            .saturating_sub(3);
        line.resize(line.len().saturating_add(filler), b'x');
        line.extend_from_slice(b"\"}\n");
        line
    }

    fn text_of<'a>(message: &'a Value, field: &str) -> &'a str {
        message[field].as_str().expect("the field is a string")
    }

    fn replay(case: &Value) {
        let name = case["name"].as_str().unwrap_or_default();
        let directory = tempfile::tempdir().expect("directory must create");
        let path = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        let agent = ConnectedAgent::listen(&path).expect("listen");
        let mut peer: Option<Peer> = None;
        let mut turns = BTreeMap::<String, JoinHandle<_>>::new();
        let mut answers = BTreeMap::<String, String>::new();
        let deadline =
            Instant::now().checked_add(Duration::from_secs(5)).expect("deadline");
        for step in case["steps"].as_array().expect("steps") {
            let ignored = step["ignored"] == true;
            if let Some(message) = step.get("agent") {
                if message["type"] == "ready" {
                    peer = Some(Peer::attach(&agent, &path, message));
                    continue;
                }
                if message["type"] == "result" && !ignored {
                    answers.insert(
                        text_of(message, "id").to_owned(),
                        text_of(message, "text").to_owned(),
                    );
                }
                peer.as_mut().expect("a connection").write(message);
            } else if let Some(message) = step.get("node") {
                let id = text_of(message, "id").to_owned();
                let mut expected = message.clone();
                if message["type"] == "task" {
                    let text = text_of(message, "text").to_owned();
                    if ignored {
                        assert_eq!(
                            agent.clone().start_turn(&turn_ctx(&id, &text, deadline)),
                            Err(ExecutionError::Protocol),
                            "{name}"
                        );
                        continue;
                    }
                    expected["instructions"] = json!(agent_instructions(GENERAL_SKILL));
                    let mut runner = agent.clone();
                    let ctx = turn_ctx(&id, &text, deadline);
                    turns.insert(
                        id.clone(),
                        std::thread::spawn(move || runner.start_turn(&ctx)),
                    );
                } else {
                    agent.cancel(&id);
                }
                assert_eq!(
                    peer.as_mut().expect("a connection").read(),
                    expected,
                    "{name}"
                );
            } else if let Some(line) = step.get("agent_line") {
                peer.as_mut()
                    .expect("a connection")
                    .write_json_line(line.as_str().expect("a line"));
            } else if let Some(bytes) = step.get("agent_line_bytes") {
                let line = noise(bytes.as_u64().expect("a size"));
                let _ = peer.as_mut().expect("a connection").stream.write_all(&line);
            } else if step["closes"] == "node" {
                assert!(peer.take().expect("a connection").closed(), "{name}");
            } else if step["closes"] == "agent" {
                peer = None;
                // The node sees the close before the next step: an agent
                // that attached earlier would replace the connection and
                // cancel its turns.
                while agent.generation().is_some() {
                    assert!(Instant::now() < deadline, "{name}: the node must detach");
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
        for (id, outcome) in case["outcomes"].as_object().expect("outcomes") {
            let expected = match outcome.as_str() {
                Some("answered") => Ok(answers.get(id).cloned().expect("an answer")),
                Some("cancelled") => Err(ExecutionError::Cancelled),
                Some("failed" | "lost") => Err(ExecutionError::Unavailable),
                other => panic!("unknown outcome {other:?}"),
            };
            let turn = turns.remove(id).expect("a turn");
            assert_eq!(turn.join().expect("turn joins"), expected, "{name}: {id}");
        }
    }

    #[test]
    fn the_node_replays_the_protocol_transcript() {
        for case in json_file(TRANSCRIPT)["cases"].as_array().expect("cases") {
            replay(case);
        }
    }

    #[test]
    fn the_constants_and_messages_match_the_contract() {
        let contract = json_file(CONTRACT);
        assert_eq!(contract["protocol"], PROTOCOL);
        assert_eq!(contract["socket_file"], crate::data_dir::AGENT_SOCKET);
        assert_eq!(contract["max_line_bytes"], MAX_LINE);

        let fields = |message: Value| -> Vec<String> {
            let mut keys: Vec<String> = message
                .as_object()
                .expect("an object")
                .keys()
                .filter(|key| *key != "type")
                .cloned()
                .collect();
            keys.sort();
            keys
        };
        let sorted = |names: &Value| -> Vec<String> {
            let mut names: Vec<String> = names
                .as_array()
                .expect("field names")
                .iter()
                .map(|name| name.as_str().expect("a name").to_owned())
                .collect();
            names.sort();
            names
        };
        let task = NodeMessage::Task { id: "t", instructions: String::new(), text: "" };
        let cancel = NodeMessage::Cancel { id: "t" };
        let node = &contract["messages"]["node_to_agent"];
        assert_eq!(fields(json!(task)), sorted(&node["task"]));
        assert_eq!(fields(json!(cancel)), sorted(&node["cancel"]));

        for (kind, names) in
            contract["messages"]["agent_to_node"].as_object().expect("messages")
        {
            let mut message = json!({ "type": kind });
            for name in names.as_array().expect("field names") {
                let name = name.as_str().expect("a name");
                message[name] =
                    if name == "protocol" { json!(PROTOCOL) } else { json!("x") };
            }
            assert!(
                serde_json::from_value::<AgentMessage>(message).is_ok(),
                "the node reads {kind}"
            );
        }
    }

    #[test]
    fn the_socket_is_owner_only() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        let _agent = ConnectedAgent::listen(&path).expect("listen");
        let mode =
            std::fs::metadata(&path).expect("socket exists").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn no_attached_agent_is_unavailable() {
        let directory = tempfile::tempdir().expect("directory must create");
        let mut agent = ConnectedAgent::listen(
            &crate::data_dir::DataDir::new(directory.path()).agent_socket(),
        )
        .expect("listen");
        let deadline =
            Instant::now().checked_add(Duration::from_secs(5)).expect("deadline");
        assert_eq!(
            agent.start_turn(&turn_ctx("task-1", "hello", deadline)),
            Err(ExecutionError::Unavailable)
        );
    }

    #[test]
    fn a_turn_past_its_time_limit_times_out_and_is_cancelled() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        let mut agent = ConnectedAgent::listen(&path).expect("listen");
        let mut peer =
            Peer::attach(&agent, &path, &json!({"type": "ready", "protocol": 1}));
        let deadline =
            Instant::now().checked_add(Duration::from_millis(50)).expect("deadline");
        assert_eq!(
            agent.start_turn(&turn_ctx("task-1", "wait", deadline)),
            Err(ExecutionError::Timeout)
        );
        assert_eq!(peer.read()["type"], "task");
        assert_eq!(peer.read(), json!({"type": "cancel", "id": "task-1"}));
    }

    #[test]
    fn a_replaced_agent_is_closed_and_its_turns_fail() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        let agent = ConnectedAgent::listen(&path).expect("listen");
        let ready = json!({"type": "ready", "protocol": 1});
        let mut first = Peer::attach(&agent, &path, &ready);
        let mut runner = agent.clone();
        let deadline =
            Instant::now().checked_add(Duration::from_secs(5)).expect("deadline");
        let turn = std::thread::spawn(move || {
            runner.start_turn(&turn_ctx("task-1", "hello", deadline))
        });
        assert_eq!(first.read()["id"], "task-1");

        let mut second = Peer::attach(&agent, &path, &ready);
        assert_eq!(first.read(), json!({"type": "cancel", "id": "task-1"}));
        assert!(first.closed());
        assert_eq!(turn.join().expect("turn joins"), Err(ExecutionError::Cancelled));

        let mut runner = agent;
        let turn = std::thread::spawn(move || {
            runner.start_turn(&turn_ctx("task-2", "hello", deadline))
        });
        assert_eq!(second.read()["id"], "task-2");
        second.write(&json!({"type": "result", "id": "task-2", "text": "answer"}));
        assert_eq!(turn.join().expect("turn joins"), Ok("answer".to_owned()));
    }

    #[test]
    fn cancel_before_turn_registration_is_honored() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        let mut agent = ConnectedAgent::listen(&path).expect("listen");
        let _peer =
            Peer::attach(&agent, &path, &json!({"type": "ready", "protocol": 1}));
        agent.cancel("task-early");
        let deadline =
            Instant::now().checked_add(Duration::from_secs(5)).expect("deadline");
        assert_eq!(
            agent.start_turn(&turn_ctx("task-early", "hello", deadline)),
            Err(ExecutionError::Cancelled)
        );
    }

    #[test]
    fn a_cancel_for_a_run_that_started_no_turn_does_not_stop_the_next_run() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        let agent = ConnectedAgent::listen(&path).expect("listen");
        let mut peer =
            Peer::attach(&agent, &path, &json!({"type": "ready", "protocol": 1}));
        peer.stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout must set");
        // The lease renewal of a run failed before its turn started, and
        // the run ended.
        agent.cancel("task-1");
        agent.forget("task-1");
        let deadline =
            Instant::now().checked_add(Duration::from_secs(5)).expect("deadline");
        let mut runner = agent;
        let turn = std::thread::spawn(move || {
            runner.start_turn(&turn_ctx("task-1", "hello", deadline))
        });
        assert_eq!(peer.read()["type"], "task");
        peer.write(&json!({"type": "result", "id": "task-1", "text": "answer"}));
        assert_eq!(turn.join().expect("turn joins"), Ok("answer".to_owned()));
    }

    #[test]
    fn a_cancel_after_the_turn_ended_is_not_kept_for_a_later_start() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        let agent = ConnectedAgent::listen(&path).expect("listen");
        let mut peer =
            Peer::attach(&agent, &path, &json!({"type": "ready", "protocol": 1}));
        peer.stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout must set");
        let deadline =
            Instant::now().checked_add(Duration::from_secs(5)).expect("deadline");
        let answer = |peer: &mut Peer| {
            let mut runner = agent.clone();
            let turn = std::thread::spawn(move || {
                runner.start_turn(&turn_ctx("task-1", "hello", deadline))
            });
            assert_eq!(peer.read()["type"], "task");
            peer.write(&json!({"type": "result", "id": "task-1", "text": "answer"}));
            turn.join().expect("turn joins")
        };
        assert_eq!(answer(&mut peer), Ok("answer".to_owned()));
        // The node may cancel a task that already ended; that cancel is
        // not an early one for a turn that starts later.
        agent.cancel("task-1");
        assert_eq!(answer(&mut peer), Ok("answer".to_owned()));
    }

    #[test]
    fn a_cancel_never_reaches_the_agent_before_its_task() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        let agent = ConnectedAgent::listen(&path).expect("listen");
        let mut peer =
            Peer::attach(&agent, &path, &json!({"type": "ready", "protocol": 1}));
        peer.stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .expect("timeout must set");
        let deadline =
            Instant::now().checked_add(Duration::from_secs(5)).expect("deadline");
        for round in 0..100 {
            let id = format!("task-{round}");
            let mut runner = agent.clone();
            let ctx = turn_ctx(&id, "hello", deadline);
            let turn = std::thread::spawn(move || runner.start_turn(&ctx));
            // The cancels race with the start of the turn. One that comes
            // before the start is kept and no task is sent.
            let canceller = agent.clone();
            let task = id.clone();
            let cancels = std::thread::spawn(move || {
                for _ in 0..20 {
                    canceller.cancel(&task);
                }
            });
            cancels.join().expect("cancels join");
            // A marker turn ends the lines of this round.
            let mut marker = agent.clone();
            let mark = std::thread::spawn(move || {
                marker.start_turn(&turn_ctx("marker", "hello", deadline))
            });
            let mut first = None;
            loop {
                let line = peer.read();
                if line["id"] == "marker" {
                    break;
                }
                first.get_or_insert(line);
            }
            if let Some(first) = first {
                assert_eq!(first["type"], "task", "round {round}");
                peer.write(&json!({"type": "cancelled", "id": id}));
            }
            peer.write(&json!({"type": "result", "id": "marker", "text": "m"}));
            assert_eq!(mark.join().expect("marker joins"), Ok("m".to_owned()));
            assert_eq!(
                turn.join().expect("turn joins"),
                Err(ExecutionError::Cancelled)
            );
        }
    }

    #[test]
    fn a_connection_that_ended_takes_no_turn() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        let agent = ConnectedAgent::listen(&path).expect("listen");
        let peer =
            Peer::attach(&agent, &path, &json!({"type": "ready", "protocol": 1}));
        // What a turn holds between `connection()` and its insert.
        let (_, taken) = agent.connection().expect("a connection");
        // The agent closes only its sending side: it still reads.
        peer.stream.shutdown(std::net::Shutdown::Write).expect("half close");
        let deadline =
            Instant::now().checked_add(Duration::from_secs(5)).expect("deadline");
        while !taken.lock().expect("turns lock").closed {
            assert!(Instant::now() < deadline, "the connection must close");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_replaced_connection_takes_no_turn() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        let agent = ConnectedAgent::listen(&path).expect("listen");
        let ready = json!({"type": "ready", "protocol": 1});
        let _first = Peer::attach(&agent, &path, &ready);
        let (_, taken) = agent.connection().expect("a connection");
        let _second = Peer::attach(&agent, &path, &ready);
        // `attach` returns when the new connection is in place; the old
        // one was closed for turns before that.
        assert!(taken.lock().expect("turns lock").closed);
    }

    #[test]
    fn a_file_that_is_not_a_socket_is_kept() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        std::fs::write(&path, "owner data").expect("file must write");
        assert!(ConnectedAgent::listen(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).ok().as_deref(), Some("owner data"));
    }
}
