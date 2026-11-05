//! The connected-agent adapter: an agent that runs on its own answers the
//! tasks that the node sends to its agent socket.
//!
//! The Hermes and `OpenClaw` plugins are such agents. The rules, the constants
//! and the transcript that the tests replay are in `protocol/agent-socket/`.

use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Read, Write},
    net::Shutdown,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::execution::{Agent, ExecutionError, ExecutionEvent, agent_instructions};

const PROTOCOL: u64 = 1;

const SOCKET_FILE: &str = "agent.sock";

/// The longest line the node reads from an agent, its newline included: a
/// final answer is bounded again by the executor's context limit.
const MAX_LINE: u64 = 4 * 1024 * 1024;

/// The path of the agent socket of a node data directory.
#[must_use]
pub fn socket_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SOCKET_FILE)
}

#[derive(Clone)]
pub struct ConnectedAgent {
    shared: Arc<Shared>,
}

struct Shared {
    connection: Mutex<Option<Connection>>,
    generation: AtomicU64,
    timeout: Duration,
}

/// The node's end of the connection, shared by the turns that write to it.
type Writer = Arc<Mutex<UnixStream>>;

/// The turns that wait for an answer, by task ID.
type Pending = Arc<Mutex<BTreeMap<String, mpsc::Sender<Reply>>>>;

struct Connection {
    generation: u64,
    /// Closes the connection when a newer agent replaces it.
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
    /// Listens on `path` for the agent. A stale socket file from an earlier
    /// run is replaced; any other file there is an error. Each turn may run
    /// for at most `timeout`.
    pub fn listen(path: &Path, timeout: Duration) -> io::Result<Self> {
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
                timeout,
            }),
        };
        let shared = Arc::clone(&agent.shared);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || shared.serve(stream));
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

    /// The generation of the attached connection, if one is attached.
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
    /// Runs one agent connection until it closes. The newest agent that
    /// says `ready` replaces an older one: the older connection closes, and
    /// its turns fail.
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
            // Its own serve thread then reads the end of the stream and
            // fails its turns.
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
                // The turn has ended when its receiver is gone.
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
        // Dropping the senders ends every waiting turn as unavailable.
        if let Ok(mut pending) = pending.lock() {
            pending.clear();
        }
    }
}

/// Reads the next message and skips lines that are not one. `None` at the
/// end of the stream, on a read error and on a line that is too long.
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
    fn start_turn(
        &mut self,
        task_id: &str,
        skill: &str,
        remote_text: &str,
    ) -> Result<String, ExecutionError> {
        self.start_turn_with_events(task_id, skill, remote_text, &mut |_| {})
    }

    fn start_turn_with_events(
        &mut self,
        task_id: &str,
        skill: &str,
        remote_text: &str,
        events: &mut dyn FnMut(ExecutionEvent),
    ) -> Result<String, ExecutionError> {
        let (writer, pending) = self.connection()?;
        let (sender, receiver) = mpsc::channel();
        {
            let mut pending =
                pending.lock().map_err(|_| ExecutionError::Unavailable)?;
            if pending.contains_key(task_id) {
                return Err(ExecutionError::Protocol);
            }
            pending.insert(task_id.to_owned(), sender);
        }
        let result = (|| {
            write_line(
                &writer,
                &NodeMessage::Task {
                    id: task_id,
                    instructions: agent_instructions(skill),
                    text: remote_text,
                },
            )?;
            let started_at = Instant::now();
            loop {
                let remaining = self
                    .shared
                    .timeout
                    .checked_sub(started_at.elapsed())
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
            pending.remove(task_id);
        }
        if result == Err(ExecutionError::Timeout) {
            // A failed write means the connection is gone, and the agent
            // forgets its turns then.
            let _ = write_line(&writer, &NodeMessage::Cancel { id: task_id });
        }
        result
    }

    fn cancel(&self, task_id: &str) {
        let Ok((writer, pending)) = self.connection() else { return };
        let active = pending.lock().is_ok_and(|pending| pending.contains_key(task_id));
        if active {
            let _ = write_line(&writer, &NodeMessage::Cancel { id: task_id });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentMessage, ConnectedAgent, MAX_LINE, NodeMessage, PROTOCOL, SOCKET_FILE,
        socket_path,
    };
    use crate::execution::{Agent, ExecutionError, GENERAL_SKILL, agent_instructions};
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

    /// The test's end of one agent connection.
    struct Peer {
        stream: UnixStream,
        lines: BufReader<UnixStream>,
    }

    impl Peer {
        /// Connects, writes `ready`, and waits until the node has attached
        /// this connection.
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

        /// Whether the node has closed the connection.
        fn closed(&mut self) -> bool {
            let mut rest = Vec::new();
            match self.lines.read_to_end(&mut rest) {
                Ok(_) => rest.is_empty(),
                Err(_) => true,
            }
        }
    }

    /// A line of exactly `bytes` bytes, the newline included, of a type
    /// that no side knows.
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

    /// Replays one transcript case with the node under test.
    fn replay(case: &Value) {
        let name = case["name"].as_str().unwrap_or_default();
        let directory = tempfile::tempdir().expect("directory must create");
        let path = socket_path(directory.path());
        let agent =
            ConnectedAgent::listen(&path, Duration::from_secs(5)).expect("listen");
        let mut peer: Option<Peer> = None;
        let mut turns = BTreeMap::<String, JoinHandle<_>>::new();
        let mut answers = BTreeMap::<String, String>::new();
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
                            agent.clone().start_turn(&id, GENERAL_SKILL, &text),
                            Err(ExecutionError::Protocol),
                            "{name}"
                        );
                        continue;
                    }
                    expected["instructions"] = json!(agent_instructions(GENERAL_SKILL));
                    let mut runner = agent.clone();
                    let turn_id = id.clone();
                    turns.insert(
                        id,
                        std::thread::spawn(move || {
                            runner.start_turn(&turn_id, GENERAL_SKILL, &text)
                        }),
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
                // The node may close before the whole line is read.
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
        assert_eq!(contract["socket_file"], SOCKET_FILE);
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
        let path = socket_path(directory.path());
        let _agent =
            ConnectedAgent::listen(&path, Duration::from_secs(5)).expect("listen");
        let mode =
            std::fs::metadata(&path).expect("socket exists").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn no_attached_agent_is_unavailable() {
        let directory = tempfile::tempdir().expect("directory must create");
        let mut agent = ConnectedAgent::listen(
            &socket_path(directory.path()),
            Duration::from_secs(5),
        )
        .expect("listen");
        assert_eq!(
            agent.start_turn("task-1", GENERAL_SKILL, "hello"),
            Err(ExecutionError::Unavailable)
        );
    }

    #[test]
    fn a_turn_past_its_time_limit_times_out_and_is_cancelled() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = socket_path(directory.path());
        let mut agent =
            ConnectedAgent::listen(&path, Duration::from_millis(50)).expect("listen");
        let mut peer =
            Peer::attach(&agent, &path, &json!({"type": "ready", "protocol": 1}));
        assert_eq!(
            agent.start_turn("task-1", GENERAL_SKILL, "wait"),
            Err(ExecutionError::Timeout)
        );
        assert_eq!(peer.read()["type"], "task");
        assert_eq!(peer.read(), json!({"type": "cancel", "id": "task-1"}));
    }

    #[test]
    fn a_replaced_agent_is_closed_and_its_turns_fail() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = socket_path(directory.path());
        let agent =
            ConnectedAgent::listen(&path, Duration::from_secs(5)).expect("listen");
        let ready = json!({"type": "ready", "protocol": 1});
        let mut first = Peer::attach(&agent, &path, &ready);
        let mut runner = agent.clone();
        let turn = std::thread::spawn(move || {
            runner.start_turn("task-1", GENERAL_SKILL, "hello")
        });
        assert_eq!(first.read()["id"], "task-1");

        let mut second = Peer::attach(&agent, &path, &ready);
        assert!(first.closed());
        assert_eq!(turn.join().expect("turn joins"), Err(ExecutionError::Unavailable));

        let mut runner = agent;
        let turn = std::thread::spawn(move || {
            runner.start_turn("task-2", GENERAL_SKILL, "hello")
        });
        assert_eq!(second.read()["id"], "task-2");
        second.write(&json!({"type": "result", "id": "task-2", "text": "answer"}));
        assert_eq!(turn.join().expect("turn joins"), Ok("answer".to_owned()));
    }

    #[test]
    fn a_file_that_is_not_a_socket_is_kept() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = socket_path(directory.path());
        std::fs::write(&path, "owner data").expect("file must write");
        assert!(ConnectedAgent::listen(&path, Duration::from_secs(5)).is_err());
        assert_eq!(std::fs::read_to_string(&path).ok().as_deref(), Some("owner data"));
    }
}
