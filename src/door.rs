//! warp-flow's door: a loopback, token-authenticated NDJSON server on std
//! threads.
//!
//! The wire is the family's ([ADR-0012], `warpui_kit_platform::door`): the
//! first line is `{"auth": "<token>"}`; a wrong token answers
//! `{"ok": false, "error": "auth"}` and closes; every line after that is one
//! request, `{"id": <int>, "cmd": "<verb>", ...params}`, and gets exactly one
//! answer carrying the same `id`, `{"id": <int>, "ok": true, ...payload}` or
//! `{"id": <int>, "ok": false, "error": "<sentence>"}`. A bad JSON line, a
//! missing `id` and an over-long line are error frames for `id: 0`, never a
//! panic and never a dropped connection — only the client leaves.
//!
//! [`PROTOCOL`] is warp-flow's own version, not warp-agent's: ADR-0012 says
//! each door's version moves with its own client. The schema of the documents
//! this door answers is [`SCHEMA_VERSION`].
//!
//! The seven verbs are the ADR's — [`cmd::RUNS`], [`cmd::RUN`],
//! [`cmd::ANSWER`], [`cmd::PAUSE`], [`cmd::RESUME`], [`cmd::ABANDON`] and
//! [`cmd::SUBSCRIBE`] — with [`cmd::PING`] for a liveness probe.
//! Board content is pull-only: `runs` answers the snapshot the window is
//! showing, and a subscriber hears `{"push": "board"}` (no payload) so it can
//! ask again. A run's payload rides whole, because the run screen wants it
//! live: `{"push": "run", "run": <RunSnapshot>}` whenever a host run's phase,
//! status or log changes. One subscription may be unfiltered or filtered to
//! one issue folder; a board change is global, so every subscriber hears it.
//! A subscribed connection is a stream from then on: no more requests are
//! read on it (the client opens a second connection for verbs).
//!
//! The app has no async runtime of its own, so this is std threads, the way
//! the family's blocking work is done: one thread for `accept`, one per
//! connection (with a writer thread fed by a channel), and one watcher that
//! diffs [`Host::snapshot`] every [`host::POLL_INTERVAL`] and fans changes out
//! through a registry of subscribers. The window publishes the snapshot it
//! adopted into a [`SharedBoard`], so the door answers what the window shows
//! and never runs a scan of its own: one source of truth.
//!
//! The door is never a reason for the app to fail: [`Door::start`] returns a
//! bind/publish error for the caller to log, the window keeps working with the
//! door down, and the endpoint files are the client's problem. The token is
//! regenerated every launch, so a stale file grants nothing.
//!
//! [ADR-0012]: https://github.com/enginkirmaci/warp-suite/blob/main/docs/adr/0012-warp-flow-publishes-a-door-of-its-own.md

use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use warpui_kit_platform::door as kit;

use crate::host::{self, AnswerOutcome, Host, Phase};
use crate::paths::AppPaths;
use crate::store::Snapshot;

/// warp-flow's wire version: the verbs and frames in this module. It is its
/// own number (not warp-agent's `PROTOCOL`): each door's version moves with
/// its own client.
pub const PROTOCOL: u32 = 1;

/// The schema version of every document this door answers: `runs`, `run` and
/// the host snapshot.
pub const SCHEMA_VERSION: u32 = 1;

/// How often the watcher diffs the host's in-memory snapshot and the board
/// handle. This is a memory copy and a comparison, not a scan.
const WATCH_INTERVAL: Duration = host::POLL_INTERVAL;

/// How long [`Door::close`] waits for its threads to leave. A blocking engine
/// child is not the door's to kill; a bounded wait keeps the app's quit path
/// moving.
const CLOSE_TIMEOUT: Duration = Duration::from_millis(500);

/// How long a door thread sleeps between checks of its stop flag. Short enough
/// that [`Door::close`] joins a sleeping watcher promptly, because the quit
/// callback runs it synchronously.
const STOP_SLICE: Duration = Duration::from_millis(25);

/// The verb vocabulary: a typo in a server or a probe breaks the build, not
/// the runtime.
pub mod cmd {
    pub const PING: &str = "ping";
    pub const RUNS: &str = "runs";
    pub const RUN: &str = "run";
    pub const ANSWER: &str = "answer";
    pub const PAUSE: &str = "pause";
    pub const RESUME: &str = "resume";
    pub const ABANDON: &str = "abandon";
    pub const SUBSCRIBE: &str = "subscribe";
}

/// The door endpoint as currently published; None until an instance binds.
pub fn read_endpoint() -> Option<(u16, String)> {
    kit::read_endpoint_in(&AppPaths::config_dir())
}

/// The board the window is showing, shared with the door.
///
/// The scan adopts a changed snapshot, the window publishes it here, and the
/// door's `runs` verb answers it — the door never scans. A revision counter
/// keeps the watcher from cloning a whole board every 250 ms just to find out
/// that nothing changed: [`SharedBoard::publish`] is the only writer and bumps
/// it.
#[derive(Clone, Default)]
pub struct SharedBoard {
    inner: Arc<BoardInner>,
}

#[derive(Default)]
struct BoardInner {
    value: Mutex<Option<Snapshot>>,
    revision: AtomicU64,
}

impl SharedBoard {
    /// A board with nothing published yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Replace the published board and mark it changed. Called on the window's
    /// scan-adopt path, never from a frame.
    pub fn publish(&self, snapshot: Snapshot) {
        *lock(&self.inner.value) = Some(snapshot);
        self.inner.revision.fetch_add(1, Ordering::SeqCst);
    }

    /// The published board, cloned for one answer. `None` until the first
    /// scan adopts something.
    pub fn read(&self) -> Option<Snapshot> {
        lock(&self.inner.value).clone()
    }

    /// How many times the board was published; the watcher's change check.
    pub fn revision(&self) -> u64 {
        self.inner.revision.load(Ordering::SeqCst)
    }
}

/// One subscriber's stream: the issue it follows (`None` is everything) and
/// the channel its frames go down.
struct Subscriber {
    issue: Option<PathBuf>,
    lines: Sender<String>,
}

/// The bound door. Held by the window and closed on quit.
pub struct Door {
    inner: Arc<Inner>,
}

/// Everything the threads share. The listener itself lives in the accept
/// thread; the port is all `close` needs to wake it.
struct Inner {
    dir: PathBuf,
    port: u16,
    token: String,
    host: Arc<Host>,
    board: SharedBoard,
    running: AtomicBool,
    subscribers: Mutex<Vec<Subscriber>>,
    writers: Mutex<Vec<JoinHandle<()>>>,
    accept: Mutex<Option<JoinHandle<()>>>,
    watch: Mutex<Option<JoinHandle<()>>>,
}

impl Door {
    /// Bind the door on a loopback port and publish `ipc.token` then
    /// `ipc.port` under `~/.warp/flow`.
    pub fn start(host: Arc<Host>, board: SharedBoard) -> std::io::Result<Self> {
        Self::start_in(AppPaths::config_dir(), host, board)
    }

    /// [`Door::start`] with an explicit publish directory: the probe seam, so
    /// no test writes into a developer's `~/.warp`.
    pub fn start_in(dir: PathBuf, host: Arc<Host>, board: SharedBoard) -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let token = kit::random_token();
        std::fs::create_dir_all(&dir)?;
        publish_endpoint(&dir, port, &token)?;
        let inner = Arc::new(Inner {
            dir,
            port,
            token,
            host,
            board,
            running: AtomicBool::new(true),
            subscribers: Mutex::new(Vec::new()),
            writers: Mutex::new(Vec::new()),
            accept: Mutex::new(None),
            watch: Mutex::new(None),
        });

        let accept = {
            let inner = Arc::clone(&inner);
            thread::Builder::new()
                .name("warp-flow-door".to_owned())
                .spawn(move || accept_loop(inner, listener))
        };
        let accept = match accept {
            Ok(handle) => handle,
            Err(error) => {
                remove_endpoint(&inner.dir, inner.port, &inner.token);
                return Err(error);
            }
        };
        *lock(&inner.accept) = Some(accept);

        let watch = {
            let inner = Arc::clone(&inner);
            thread::Builder::new()
                .name("warp-flow-door-watch".to_owned())
                .spawn(move || watch_loop(inner))
        };
        match watch {
            Ok(handle) => *lock(&inner.watch) = Some(handle),
            Err(error) => {
                // The accept thread is already running, so take the endpoint
                // down before reporting the failure.
                inner.running.store(false, Ordering::SeqCst);
                wake_accept(&inner);
                if let Some(handle) = lock(&inner.accept).take() {
                    join_within(handle, Instant::now() + CLOSE_TIMEOUT);
                }
                remove_endpoint(&inner.dir, inner.port, &inner.token);
                return Err(error);
            }
        }
        Ok(Self { inner })
    }

    /// The bound loopback port.
    pub fn port(&self) -> u16 {
        self.inner.port
    }

    /// The token this launch published.
    pub fn token(&self) -> &str {
        &self.inner.token
    }

    /// Stop the listener, drop the subscribers and join the door's threads
    /// with a bounded wait. Idempotent: the window's quit path and a probe may
    /// both call it.
    pub fn close(&self) {
        let inner = &self.inner;
        inner.running.store(false, Ordering::SeqCst);
        // std has no listener shutdown; one loopback connection of our own
        // wakes the blocking `accept`, and its loop then sees `running` off.
        wake_accept(inner);
        // Dropping the senders closes each writer's channel; the writer drains
        // what is queued, sends EOF and leaves.
        lock(&inner.subscribers).clear();
        let deadline = Instant::now() + CLOSE_TIMEOUT;
        if let Some(handle) = lock(&inner.accept).take() {
            join_within(handle, deadline);
        }
        if let Some(handle) = lock(&inner.watch).take() {
            join_within(handle, deadline);
        }
        let writers: Vec<JoinHandle<()>> = lock(&inner.writers).drain(..).collect();
        for handle in writers {
            join_within(handle, deadline);
        }
    }

    /// Remove the published endpoint, but only when the files still name this
    /// instance: a newer launch owns the directory then.
    pub fn unpublish(&self) {
        remove_endpoint(&self.inner.dir, self.inner.port, &self.inner.token);
    }
}

/// Publish the endpoint: token first, because a reader that sees a port is
/// entitled to its token, and both files private from the open (see
/// [`kit::write_private`]).
fn publish_endpoint(dir: &Path, port: u16, token: &str) -> std::io::Result<()> {
    let token_path = kit::token_file_in(dir);
    kit::write_private(&token_path, token)?;
    kit::restrict_perms(&token_path);
    let port_path = kit::port_file_in(dir);
    if let Err(error) = kit::write_private(&port_path, &format!("{port}\n")) {
        // A port-less token file is no endpoint: do not leave half a
        // publication behind.
        let _ = std::fs::remove_file(&token_path);
        return Err(error);
    }
    kit::restrict_perms(&port_path);
    Ok(())
}

/// Remove the endpoint files only when they still hold this instance's port
/// and token.
fn remove_endpoint(dir: &Path, port: u16, token: &str) {
    let ours = |file: &Path, value: &str| {
        std::fs::read_to_string(file).is_ok_and(|current| current.trim() == value)
    };
    let port = port.to_string();
    if ours(&kit::port_file_in(dir), &port) {
        let _ = std::fs::remove_file(kit::port_file_in(dir));
    }
    if ours(&kit::token_file_in(dir), token) {
        let _ = std::fs::remove_file(kit::token_file_in(dir));
    }
}

/// Close our own loopback connection so a blocking `accept` returns.
fn wake_accept(inner: &Inner) {
    let _ = TcpStream::connect(("127.0.0.1", inner.port));
}

fn accept_loop(inner: Arc<Inner>, listener: TcpListener) {
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                if !inner.running.load(Ordering::SeqCst) {
                    break;
                }
                let connection = Arc::clone(&inner);
                if let Err(error) = thread::Builder::new()
                    .name("warp-flow-door-conn".to_owned())
                    .spawn(move || handle_connection(connection, stream))
                {
                    tracing::warn!("warp-flow's door could not serve a connection: {error}");
                }
            }
            Err(error) => {
                if !inner.running.load(Ordering::SeqCst) {
                    break;
                }
                tracing::warn!("warp-flow's door could not accept a connection: {error}");
            }
        }
    }
}

/// The watcher: the door learns that a run or the board changed on its own,
/// so the render path never has to tell it anything. It copies the host's
/// in-memory snapshot and compares revisions of the board handle; a changed
/// run is pushed whole, a changed board as a contentless notification.
fn watch_loop(inner: Arc<Inner>) {
    let mut last_host = host::HostSnapshot::default();
    let mut last_board = inner.board.revision();
    while sleep_until_stopped(&inner, WATCH_INTERVAL) {
        let next = inner.host.snapshot();
        for run in &next.runs {
            if last_host.run(&run.issue) == Some(run) {
                continue;
            }
            match serde_json::to_value(run) {
                Ok(value) => {
                    let frame = json!({"push": "run", "run": value}).to_string();
                    fan_out(&inner, Some(&run.issue), &frame);
                }
                Err(error) => {
                    tracing::warn!("warp-flow's door could not publish a run change: {error}");
                }
            }
        }
        last_host = next;
        let revision = inner.board.revision();
        if revision != last_board {
            last_board = revision;
            fan_out(&inner, None, &json!({"push": "board"}).to_string());
        }
    }
}

/// Sleep for `duration` in short slices, returning false as soon as the door
/// is asked to stop. A single long sleep would make [`Door::close`] wait for
/// it.
fn sleep_until_stopped(inner: &Inner, duration: Duration) -> bool {
    let mut slept = Duration::ZERO;
    while slept < duration {
        if !inner.running.load(Ordering::SeqCst) {
            return false;
        }
        let slice = STOP_SLICE.min(duration - slept);
        thread::sleep(slice);
        slept += slice;
    }
    inner.running.load(Ordering::SeqCst)
}

/// Fan one frame out to the subscribers it concerns. `None` means a global
/// frame (the board notification) and goes to every subscriber; `Some(issue)`
/// is a run change and skips subscriptions filtered to another issue. A
/// subscriber whose channel is closed has left and is dropped.
fn fan_out(inner: &Inner, issue: Option<&Path>, frame: &str) {
    let mut subscribers = lock(&inner.subscribers);
    subscribers.retain(|subscriber| {
        let wanted = issue.is_none_or(|issue| {
            subscriber
                .issue
                .as_deref()
                .is_none_or(|filter| filter == issue)
        });
        if !wanted {
            return true;
        }
        subscriber.lines.send(frame.to_owned()).is_ok()
    });
}

/// One connection: an auth line, then one request at a time until the client
/// leaves (or, after `subscribe`, until the connection closes).
fn handle_connection(inner: Arc<Inner>, stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    let Ok(write_half) = stream.try_clone() else {
        return;
    };
    let (lines, incoming) = mpsc::channel::<String>();
    let writer = thread::Builder::new()
        .name("warp-flow-door-writer".to_owned())
        .spawn(move || write_loop(write_half, incoming));
    let Ok(writer) = writer else {
        return;
    };
    lock(&inner.writers).push(writer);

    let mut reader = BufReader::new(stream);
    let first = read_frame(&mut reader);
    if matches!(first, Frame::Eof) {
        return;
    }
    let authed = match &first {
        Frame::Line(line) => serde_json::from_str::<Value>(line)
            .ok()
            .and_then(|frame| frame.get("auth").and_then(Value::as_str).map(str::to_owned))
            .is_some_and(|auth| kit::token_matches(&auth, &inner.token)),
        Frame::TooLong | Frame::Invalid | Frame::Eof => false,
    };
    if !authed {
        // The kit's compare is length-checked and byte-folded; the refusal is
        // the family's one frame and the connection then ends.
        let _ = lines.send(json!({"ok": false, "error": "auth"}).to_string());
        return;
    }
    if lines.send(json!({"ok": true}).to_string()).is_err() {
        return;
    }

    loop {
        match read_frame(&mut reader) {
            Frame::Eof => break,
            Frame::TooLong => {
                let frame = kit::answer_frame(
                    0,
                    Err(format!("frame exceeds the {} byte line cap", kit::MAX_LINE)),
                );
                if lines.send(frame.to_string()).is_err() {
                    break;
                }
            }
            Frame::Invalid => {
                let frame = kit::answer_frame(0, Err("frame is not valid utf-8".to_owned()));
                if lines.send(frame.to_string()).is_err() {
                    break;
                }
            }
            Frame::Line(line) => {
                // A blank line is not a request: warp-agent skips it too.
                if line.trim().is_empty() {
                    continue;
                }
                match handle_request(&inner, &line) {
                    Reply::Answer(frame) => {
                        if lines.send(frame.to_string()).is_err() {
                            break;
                        }
                    }
                    Reply::Subscribed { issue, ack } => {
                        // The ack goes out before the subscription exists, so
                        // the first pushed frame can never overtake it.
                        if lines.send(ack.to_string()).is_err() {
                            break;
                        }
                        lock(&inner.subscribers).push(Subscriber {
                            issue,
                            lines: lines.clone(),
                        });
                        // A subscribed connection is a stream: no more request
                        // lines are read on it. The client uses a second
                        // connection for verbs.
                        break;
                    }
                }
            }
        }
    }
}

/// The writer half of one connection: frames from the channel, one JSON
/// document per line, then EOF.
fn write_loop(mut stream: TcpStream, incoming: mpsc::Receiver<String>) {
    for line in incoming {
        if stream.write_all(line.as_bytes()).is_err()
            || stream.write_all(b"\n").is_err()
            || stream.flush().is_err()
        {
            break;
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// One line of the wire, as read. `TooLong` keeps the connection: the rest of
/// that line is consumed, ignored and answered with an error frame.
enum Frame {
    Line(String),
    TooLong,
    Invalid,
    Eof,
}

/// Read one NDJSON line, capped at the kit's [`kit::MAX_LINE`]. An over-long
/// line is drained to its newline instead of being buffered without bound, so
/// a hostile client cannot allocate the door's memory and the connection stays
/// usable.
fn read_frame(reader: &mut BufReader<TcpStream>) -> Frame {
    let mut line = Vec::new();
    let mut over = false;
    loop {
        let (newline, len) = {
            let chunk = match reader.fill_buf() {
                Ok(chunk) => chunk,
                Err(_) => return Frame::Eof,
            };
            if chunk.is_empty() {
                // A partial frame with the client gone is not a frame.
                return Frame::Eof;
            }
            let newline = chunk.iter().position(|byte| *byte == b'\n');
            let kept = newline.unwrap_or(chunk.len());
            if !over {
                line.extend_from_slice(&chunk[..kept]);
                over = line.len() > kit::MAX_LINE;
                if over {
                    line.clear();
                }
            }
            (newline, chunk.len())
        };
        reader.consume(newline.map_or(len, |index| index + 1));
        if newline.is_some() {
            return if over {
                Frame::TooLong
            } else {
                match String::from_utf8(line) {
                    Ok(line) => Frame::Line(line),
                    Err(_) => Frame::Invalid,
                }
            };
        }
    }
}

/// The answer to one request line: one frame, or the subscription this
/// connection becomes.
enum Reply {
    Answer(Value),
    Subscribed { issue: Option<PathBuf>, ack: Value },
}

fn handle_request(inner: &Inner, line: &str) -> Reply {
    let request = match serde_json::from_str::<Value>(line) {
        Ok(request) => request,
        Err(error) => {
            return Reply::Answer(kit::answer_frame(0, Err(format!("bad frame: {error}"))));
        }
    };
    let Some(id) = request.get("id").and_then(Value::as_u64) else {
        return Reply::Answer(kit::answer_frame(0, Err("missing id".to_owned())));
    };
    let command = request.get("cmd").and_then(Value::as_str).unwrap_or("");
    if command == cmd::SUBSCRIBE {
        return match issue_filter(&request) {
            Ok(issue) => Reply::Subscribed {
                issue,
                ack: kit::answer_frame(id, Ok(json!({"protocol": PROTOCOL}))),
            },
            Err(error) => Reply::Answer(kit::answer_frame(id, Err(error))),
        };
    }
    Reply::Answer(kit::answer_frame(id, serve(inner, command, &request)))
}

/// The optional `issue` filter of a subscription: absent (or null) is every
/// run, a path is one run, and a non-string or empty value is an error.
fn issue_filter(request: &Value) -> Result<Option<PathBuf>, String> {
    match request.get("issue") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let Some(raw) = value.as_str().map(str::trim).filter(|raw| !raw.is_empty()) else {
                return Err("issue must be a path string".to_owned());
            };
            Ok(Some(canonical(Path::new(raw))))
        }
    }
}

/// The verb table. Every failure here is one `{"ok": false, "error"}` frame
/// with the same `id`; the host's own sentence is carried through unchanged for
/// a refused pause, resume, abandon or answer.
fn serve(inner: &Inner, command: &str, request: &Value) -> Result<Value, String> {
    match command {
        cmd::PING => Ok(json!({
            "app": "warp-flow",
            "protocol": PROTOCOL,
            "schema_version": SCHEMA_VERSION,
        })),
        cmd::RUNS => serve_runs(inner),
        cmd::RUN => serve_run(inner, request),
        cmd::ANSWER => serve_answer(inner, request),
        cmd::PAUSE => drive(inner, request, Host::pause),
        cmd::RESUME => drive(inner, request, Host::resume),
        cmd::ABANDON => drive(inner, request, Host::abandon),
        other => Err(format!("unknown command: {other}")),
    }
}

/// `runs`: the board the window is showing plus the host's live runs. The
/// board is the scan's snapshot, not a second scan.
fn serve_runs(inner: &Inner) -> Result<Value, String> {
    let board = inner.board.read();
    let repos = match &board {
        Some(board) => serde_json::to_value(&board.repos)
            .map_err(|error| format!("could not serialize the board: {error}"))?,
        None => Value::Array(Vec::new()),
    };
    let host = serde_json::to_value(inner.host.snapshot())
        .map_err(|error| format!("could not serialize the host: {error}"))?;
    Ok(json!({"schema_version": SCHEMA_VERSION, "repos": repos, "host": host}))
}

/// `run`: one run's board detail (`null` only while the 2 s scan has not yet
/// adopted a folder the host already knows) and the host's live run (`null`
/// when this window never drove it). An issue in neither is the error.
fn serve_run(inner: &Inner, request: &Value) -> Result<Value, String> {
    let (named, issue) = issue_param(request)?;
    let host = inner.host.snapshot();
    let host_run = host.run(&issue).cloned();
    let board = inner.board.read();
    let board_run = board
        .as_ref()
        .and_then(|board| board.runs().find(|run| run.dir == issue));
    if host_run.is_none() && board_run.is_none() {
        return Err(format!("no run for issue `{named}`"));
    }
    let run = match board_run {
        Some(run) => serde_json::to_value(run)
            .map_err(|error| format!("could not serialize the run: {error}"))?,
        None => Value::Null,
    };
    let host_run = match &host_run {
        Some(run) => serde_json::to_value(run)
            .map_err(|error| format!("could not serialize the host run: {error}"))?,
        None => Value::Null,
    };
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "run": run,
        "host": host_run,
    }))
}

/// `answer`: the host owns the one file this app writes, so the door and the
/// run screen share one writer. The answer also continues a run this window
/// drove to its question; the host's [`AnswerOutcome`] says which happened.
fn serve_answer(inner: &Inner, request: &Value) -> Result<Value, String> {
    let (_, issue) = issue_param(request)?;
    let question = text_param(request, "question")?;
    let text = text_param(request, "text")?;
    let question = question_path(&issue, question);
    let outcome = inner.host.answer(&issue, &question, text)?;
    let outcome = match outcome {
        AnswerOutcome::Resumed => "resumed",
        AnswerOutcome::Recorded => "recorded",
    };
    Ok(json!({"outcome": outcome}))
}

/// `pause`, `resume`, `abandon`: the host's action, then the phase it left the
/// run in. A refusal is the host's own sentence in `error`.
fn drive(
    inner: &Inner,
    request: &Value,
    action: fn(&Host, &Path) -> Result<(), String>,
) -> Result<Value, String> {
    let (_, issue) = issue_param(request)?;
    action(&inner.host, &issue)?;
    Ok(json!({
        "phase": inner.host.phase(&issue).map_or("unknown", Phase::as_str),
    }))
}

/// The required `issue` parameter: the caller's spelling for messages, and the
/// canonical folder for lookups, because every run is identified by it.
fn issue_param(request: &Value) -> Result<(String, PathBuf), String> {
    let named = text_param(request, "issue")?;
    let issue = canonical(Path::new(named));
    Ok((named.to_owned(), issue))
}

/// One required non-empty string parameter, trimmed.
fn text_param<'a>(request: &'a Value, name: &str) -> Result<&'a str, String> {
    request
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{name} is required"))
}

/// Resolve the `question` parameter inside the issue folder. A client may send
/// the absolute path the board carries, `questions/01.md` as the folder spells
/// it, or just the file name.
fn question_path(issue: &Path, question: &str) -> PathBuf {
    let question = Path::new(question);
    if question.is_absolute() {
        return question.to_path_buf();
    }
    if question.components().count() > 1 {
        issue.join(question)
    } else {
        issue.join("questions").join(question)
    }
}

/// A path with its symlinks resolved when possible, so two spellings of one
/// issue folder are one key — the same rule the store's scan and the host use.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Wait for one thread to finish, at most until `deadline`; after that the
/// handle is dropped and the thread is left to the process (a blocking engine
/// has no cancellation point the door can reach).
fn join_within(handle: JoinHandle<()>, deadline: Instant) {
    while !handle.is_finished() {
        if Instant::now() >= deadline {
            return;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let _ = handle.join();
}

/// Lock a mutex, treating a poisoned lock as a plain value: a panicked
/// connection must not take the door down with it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("warp-flow-door-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn a_shared_board_reports_only_publishes_as_changes() {
        let board = SharedBoard::new();
        assert_eq!(board.revision(), 0);
        assert!(board.read().is_none());
        board.publish(Snapshot::default());
        assert_eq!(board.revision(), 1);
        assert_eq!(board.read(), Some(Snapshot::default()));
    }

    #[test]
    fn a_question_name_resolves_both_ways() {
        let issue = Path::new("/repo/.scratch/demo");
        assert_eq!(
            question_path(issue, "01.md"),
            PathBuf::from("/repo/.scratch/demo/questions/01.md")
        );
        assert_eq!(
            question_path(issue, "questions/01.md"),
            PathBuf::from("/repo/.scratch/demo/questions/01.md")
        );
        assert_eq!(
            question_path(issue, "/elsewhere/01.md"),
            PathBuf::from("/elsewhere/01.md")
        );
    }

    #[test]
    fn an_endpoint_is_published_token_first_and_removed_only_when_ours() {
        let dir = scratch_dir("endpoint");
        publish_endpoint(&dir, 51234, "token-a").expect("publish");
        assert_eq!(
            std::fs::read_to_string(kit::token_file_in(&dir)).expect("token file"),
            "token-a"
        );
        assert_eq!(
            std::fs::read_to_string(kit::port_file_in(&dir)).expect("port file"),
            "51234\n"
        );
        assert_eq!(kit::read_endpoint_in(&dir), Some((51234, "token-a".into())));

        // Another instance took the directory: this door must not remove it.
        kit::write_private(&kit::port_file_in(&dir), "51235\n").expect("other port");
        kit::write_private(&kit::token_file_in(&dir), "token-b").expect("other token");
        remove_endpoint(&dir, 51234, "token-a");
        assert_eq!(kit::read_endpoint_in(&dir), Some((51235, "token-b".into())));

        remove_endpoint(&dir, 51235, "token-b");
        assert_eq!(kit::read_endpoint_in(&dir), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
