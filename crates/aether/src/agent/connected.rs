//! The connected-agent adapter: an agent that runs on its own (the Hermes
//! gateway, for example) connects to the node's agent socket and answers
//! the tasks that the node sends it.
//!
//! The protocol is one JSON object on each line:
//!
//! - agent to node, first: `{"type":"ready","protocol":1}`;
//! - node to agent: `{"type":"task","id":…,"instructions":…,"text":…}` and
//!   `{"type":"cancel","id":…}`;
//! - agent to node: `{"type":"progress","id":…}`,
//!   `{"type":"result","id":…,"text":…}`, `{"type":"failed","id":…}` and
//!   `{"type":"cancelled","id":…}`.
//!
//! The node sends only the task text and the instructions: no keys, grants,
//! endpoint IDs or caller names. The socket file is owner-only, as the admin
//! socket is, so only the node owner's own processes can attach.

use std::{
    collections::BTreeMap,
    io::{self, BufRead, BufReader, Read, Write},
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

use serde::Deserialize;
use serde_json::json;

use crate::execution::{Agent, ExecutionError, ExecutionEvent, agent_instructions};

/// The protocol version of the agent socket.
pub const PROTOCOL: u64 = 1;

/// The file name of the agent socket in the node data directory.
pub const SOCKET_FILE: &str = "agent.sock";

/// The longest line the node reads from an agent: a final answer is bounded
/// again by the executor's context limit.
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

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Message {
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

    /// Whether an agent is attached now.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.shared.connection.lock().is_ok_and(|connection| connection.is_some())
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
}

impl Shared {
    /// Runs one agent connection until it closes. The newest agent that
    /// says `ready` replaces an older one; the older one's tasks fail.
    fn serve(&self, stream: UnixStream) {
        let Ok(writer) = stream.try_clone() else { return };
        let mut reader = BufReader::new(stream);
        match read_message(&mut reader) {
            Some(Message::Ready { protocol: PROTOCOL }) => {}
            _ => return,
        }
        let generation =
            self.generation.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let pending = Arc::new(Mutex::new(BTreeMap::new()));
        if let Ok(mut connection) = self.connection.lock() {
            *connection = Some(Connection {
                generation,
                writer: Arc::new(Mutex::new(writer)),
                pending: Arc::clone(&pending),
            });
        } else {
            return;
        }
        while let Some(message) = read_message(&mut reader) {
            let (id, reply) = match message {
                Message::Ready { .. } => continue,
                Message::Progress { id } => (id, Reply::Progress),
                Message::Result { id, text } => (id, Reply::Result(text)),
                Message::Failed { id } => (id, Reply::Failed),
                Message::Cancelled { id } => (id, Reply::Cancelled),
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
        // Dropping the senders ends every waiting turn as unavailable.
        if let Ok(mut pending) = pending.lock() {
            pending.clear();
        }
    }
}

/// Reads one message. `None` at the end of the stream, on a read error, on
/// a line that is too long or on a line that is not a known message.
fn read_message(reader: &mut BufReader<UnixStream>) -> Option<Message> {
    loop {
        let mut line = Vec::new();
        let read = reader.by_ref().take(MAX_LINE).read_until(b'\n', &mut line).ok()?;
        if read == 0 || line.last() != Some(&b'\n') {
            return None;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        return serde_json::from_slice(&line).ok();
    }
}

fn write_line(
    writer: &Mutex<UnixStream>,
    message: &serde_json::Value,
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
                &json!({
                    "type": "task",
                    "id": task_id,
                    "instructions": agent_instructions(skill),
                    "text": remote_text,
                }),
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
            let _ = write_line(&writer, &json!({"type": "cancel", "id": task_id}));
        }
        result
    }

    fn cancel(&self, task_id: &str) {
        let Ok((writer, pending)) = self.connection() else { return };
        let active = pending.lock().is_ok_and(|pending| pending.contains_key(task_id));
        if active {
            let _ = write_line(&writer, &json!({"type": "cancel", "id": task_id}));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnectedAgent, socket_path};
    use crate::execution::{Agent, ExecutionError, ExecutionEvent, GENERAL_SKILL};
    use serde_json::{Value, json};
    use std::{
        io::{BufRead, BufReader, Write},
        os::unix::{fs::PermissionsExt, net::UnixStream},
        path::Path,
        time::{Duration, Instant},
    };

    /// A test agent: connects, says `ready`, and then answers each node
    /// message with `answer`.
    fn attach(
        agent: &ConnectedAgent,
        path: &Path,
        answer: impl Fn(&Value) -> Vec<Value> + Send + 'static,
    ) {
        let mut stream = UnixStream::connect(path).expect("agent must connect");
        stream
            .write_all(b"{\"type\":\"ready\",\"protocol\":1}\n")
            .expect("ready must send");
        let reader = BufReader::new(stream.try_clone().expect("stream must clone"));
        std::thread::spawn(move || {
            for line in reader.lines() {
                let Ok(line) = line else { return };
                let message: Value =
                    serde_json::from_str(&line).expect("node sends JSON");
                for reply in answer(&message) {
                    let mut reply = serde_json::to_vec(&reply).expect("reply encodes");
                    reply.push(b'\n');
                    if stream.write_all(&reply).is_err() {
                        return;
                    }
                }
            }
        });
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("deadline must fit");
        while !agent.is_connected() {
            assert!(Instant::now() < deadline, "agent must attach");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_task_reaches_the_agent_and_its_result_comes_back() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = socket_path(directory.path());
        let mut agent =
            ConnectedAgent::listen(&path, Duration::from_secs(5)).expect("listen");
        let mode =
            std::fs::metadata(&path).expect("socket exists").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        attach(&agent, &path, |message| {
            if message["type"] != "task" {
                return Vec::new();
            }
            let id = message["id"].clone();
            let instructions = message["instructions"].as_str().unwrap_or_default();
            assert!(instructions.contains("untrusted remote task text"));
            assert_eq!(message.as_object().map(serde_json::Map::len), Some(4));
            vec![
                json!({"type": "progress", "id": id}),
                json!({"type": "result", "id": id, "text": format!("echo: {}", message["text"].as_str().unwrap_or_default())}),
            ]
        });
        let mut events = Vec::new();
        let answer = agent.start_turn_with_events(
            "task-1",
            GENERAL_SKILL,
            "hello",
            &mut |event| events.push(event),
        );
        assert_eq!(answer, Ok("echo: hello".to_owned()));
        assert_eq!(events, [ExecutionEvent::Progress, ExecutionEvent::Completed]);
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
    fn a_failed_or_cancelled_answer_carries_no_text() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = socket_path(directory.path());
        let mut agent =
            ConnectedAgent::listen(&path, Duration::from_secs(5)).expect("listen");
        attach(&agent, &path, |message| {
            let id = message["id"].clone();
            match message["text"].as_str() {
                Some("fail") => vec![json!({"type": "failed", "id": id})],
                Some(_) => vec![json!({"type": "cancelled", "id": id})],
                None => Vec::new(),
            }
        });
        assert_eq!(
            agent.start_turn("task-1", GENERAL_SKILL, "fail"),
            Err(ExecutionError::Unavailable)
        );
        assert_eq!(
            agent.start_turn("task-2", GENERAL_SKILL, "stop"),
            Err(ExecutionError::Cancelled)
        );
    }

    #[test]
    fn cancel_reaches_the_agent_while_the_turn_runs() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = socket_path(directory.path());
        let agent =
            ConnectedAgent::listen(&path, Duration::from_secs(5)).expect("listen");
        attach(&agent, &path, |message| {
            if message["type"] == "cancel" {
                vec![json!({"type": "cancelled", "id": message["id"]})]
            } else {
                Vec::new()
            }
        });
        let canceller = agent.clone();
        let mut runner = agent;
        let turn = std::thread::spawn(move || {
            runner.start_turn("task-1", GENERAL_SKILL, "wait")
        });
        std::thread::sleep(Duration::from_millis(50));
        canceller.cancel("task-1");
        assert_eq!(turn.join().expect("turn joins"), Err(ExecutionError::Cancelled));
    }

    #[test]
    fn a_turn_past_its_time_limit_times_out_and_is_cancelled() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = socket_path(directory.path());
        let mut agent =
            ConnectedAgent::listen(&path, Duration::from_millis(50)).expect("listen");
        let (sender, receiver) = std::sync::mpsc::channel();
        let sender = std::sync::Mutex::new(sender);
        attach(&agent, &path, move |message| {
            if message["type"] == "cancel" {
                let _ = sender.lock().map(|sender| sender.send(message["id"].clone()));
            }
            Vec::new()
        });
        assert_eq!(
            agent.start_turn("task-1", GENERAL_SKILL, "wait"),
            Err(ExecutionError::Timeout)
        );
        assert_eq!(receiver.recv_timeout(Duration::from_secs(5)), Ok(json!("task-1")));
    }

    #[test]
    fn a_disconnected_agent_ends_its_turns() {
        let directory = tempfile::tempdir().expect("directory must create");
        let path = socket_path(directory.path());
        let mut agent =
            ConnectedAgent::listen(&path, Duration::from_secs(5)).expect("listen");
        let mut stream = UnixStream::connect(&path).expect("agent must connect");
        stream
            .write_all(b"{\"type\":\"ready\",\"protocol\":1}\n")
            .expect("ready must send");
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(5))
            .expect("deadline must fit");
        while !agent.is_connected() {
            assert!(Instant::now() < deadline, "agent must attach");
            std::thread::sleep(Duration::from_millis(5));
        }
        let reader = BufReader::new(stream.try_clone().expect("stream must clone"));
        std::thread::spawn(move || {
            // Read the task, then hang up without an answer.
            let _ = reader.lines().next();
            drop(stream);
        });
        assert_eq!(
            agent.start_turn("task-1", GENERAL_SKILL, "hello"),
            Err(ExecutionError::Unavailable)
        );
        assert!(!agent.is_connected());
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
