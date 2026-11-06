//! The connected-agent adapter: an agent that runs on its own answers the
//! tasks that the node sends to its agent socket.
//!
//! The Hermes and `OpenClaw` plugins are such agents. The rules, the constants
//! and the transcript that the tests replay are in `protocol/agent-socket/`.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, BufRead, BufReader, Read, Write},
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
    early_cancels: Mutex<BTreeSet<String>>,
}

type Writer = Arc<Mutex<UnixStream>>;

type Pending = Arc<Mutex<BTreeMap<String, mpsc::Sender<Reply>>>>;

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
                early_cancels: Mutex::new(BTreeSet::new()),
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
        let pending = Arc::new(Mutex::new(BTreeMap::new()));
        let Ok(mut connection) = self.connection.lock() else { return };
        let replaced = connection.replace(Connection {
            generation,
            stream: control,
            writer: Arc::new(Mutex::new(writer)),
            pending: Arc::clone(&pending),
        });
        drop(connection);
        if let Some(replaced) = replaced {
            if let Ok(pending) = replaced.pending.lock() {
                for (task_id, sender) in pending.iter() {
                    let _ = write_line(
                        &replaced.writer,
                        &NodeMessage::Cancel { id: task_id.as_str() },
                    );
                    let _ = sender.send(Reply::Cancelled);
                }
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
            if let Ok(pending) = pending.lock()
                && let Some(sender) = pending.get(&id)
            {
                let _ = sender.send(reply);
            }
        }
        if let Ok(mut connection) = self.connection.lock()
            && connection
                .as_ref()
                .is_some_and(|current| current.generation == generation)
        {
            *connection = None;
        }
        if let Ok(mut pending) = pending.lock() {
            pending.clear();
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

fn write_line(
    writer: &Mutex<UnixStream>,
    message: &NodeMessage<'_>,
) -> Result<(), ExecutionError> {
    let mut writer = writer.lock().map_err(|_| ExecutionError::Unavailable)?;
    let mut line = serde_json::to_vec(message).map_err(|_| ExecutionError::Protocol)?;
    line.push(b'\n');
    writer.write_all(&line).map_err(|_| ExecutionError::Unavailable)?;
    writer.flush().map_err(|_| ExecutionError::Unavailable)
}

impl Agent for ConnectedAgent {
    fn start_turn(&mut self, ctx: &TurnContext) -> Result<String, ExecutionError> {
        self.start_turn_with_events(ctx, &mut |_| {})
    }

    fn start_turn_with_events(
        &mut self,
        ctx: &TurnContext,
        events: &mut dyn FnMut(ExecutionEvent),
    ) -> Result<String, ExecutionError> {
        if self
            .shared
            .early_cancels
            .lock()
            .map_err(|_| ExecutionError::Unavailable)?
            .remove(&ctx.task_id)
        {
            return Err(ExecutionError::Cancelled);
        }
        let (writer, pending) = self.connection()?;
        let (sender, receiver) = mpsc::channel();
        {
            let mut pending =
                pending.lock().map_err(|_| ExecutionError::Unavailable)?;
            if pending.contains_key(&ctx.task_id) {
                return Err(ExecutionError::Protocol);
            }
            pending.insert(ctx.task_id.clone(), sender);
        }
        let result = (|| {
            write_line(
                &writer,
                &NodeMessage::Task {
                    id: &ctx.task_id,
                    instructions: agent_instructions(&ctx.skill),
                    text: &ctx.text,
                },
            )?;
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
        if let Ok(mut pending) = pending.lock() {
            pending.remove(&ctx.task_id);
        }
        if result == Err(ExecutionError::Timeout) {
            write_line(&writer, &NodeMessage::Cancel { id: &ctx.task_id })?;
        }
        result
    }

    fn cancel(&self, task_id: &str) {
        if let Ok((writer, pending)) = self.connection()
            && pending.lock().is_ok_and(|pending| pending.contains_key(task_id))
        {
            let _ = write_line(&writer, &NodeMessage::Cancel { id: task_id });
            return;
        }
        if let Ok(mut early) = self.shared.early_cancels.lock() {
            early.insert(task_id.to_owned());
        }
    }

    fn supports_workspace_change(&self) -> bool {
        false
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
            self.write_line(&serde_json::to_string(message).expect("message encodes"));
        }

        fn write_line(&mut self, line: &str) {
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
                    .write_line(line.as_str().expect("a line"));
            } else if let Some(bytes) = step.get("agent_line_bytes") {
                let line = noise(bytes.as_u64().expect("a size"));
                let _ = peer.as_mut().expect("a connection").stream.write_all(&line);
            } else if step["closes"] == "node" {
                assert!(peer.take().expect("a connection").closed(), "{name}");
            } else if step["closes"] == "agent" {
                peer = None;
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
    fn a_file_that_is_not_a_socket_is_kept() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = crate::data_dir::DataDir::new(directory.path()).agent_socket();
        std::fs::write(&path, "owner data").expect("file must write");
        assert!(ConnectedAgent::listen(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).ok().as_deref(), Some("owner data"));
    }
}
