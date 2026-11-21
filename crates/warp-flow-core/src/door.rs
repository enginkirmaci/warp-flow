//! A client for warp-agent's door and the [`Harness`] over it.
//!
//! The measured contract is in `.scratch/warp-flow/spike/FINDINGS.md` in the
//! suite: subscribe before create, learn the session id from a push, `part`
//! frames are snapshots, and the transcript is the authoritative text.
//!
//! [`DoorClient`] owns one authenticated NDJSON connection and one reader
//! thread. Answer frames are routed to the request that is waiting for their
//! `id`; id-less `{"event":…}` frames are queued for
//! [`DoorClient::next_event`]. [`DoorHarness`] runs one workflow step as one
//! harness session: `harness metadata` and `models` pick the backend,
//! `harness start` attaches the event pump, a broad subscription learns the
//! new session id from the `session-changed` push (the create reply cannot
//! carry it), and a narrow subscription carries the prompt and the idle
//! status. The transcript, never the prompt echo, supplies the text.
//!
//! The client never calls `open-workspace` and never uses `run-prompt`: a
//! session created with `harness create` does not become a workspace row in
//! the owner's app, which is what the engine wants.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use crate::artifact::{Document, FrontMatter};
use crate::harness::{Harness, StepOutcome, StepRequest};
use crate::run::timestamp;
use crate::{Error, Result};

/// How long the door gets to answer one request.
///
/// Every door verb except `run-prompt` answers within 60 s (`ANSWER_TIMEOUT`
/// in warp-agent), so this client deadline sees the door's own answer instead
/// of racing it with a shorter one.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

/// How long connecting and reading the welcome may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// The cap on waiting for the push that names a freshly created session.
const SESSION_TIMEOUT: Duration = Duration::from_secs(20);

/// The cap on waiting for `session-status` `idle` after a prompt.
const IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// How long `local` waits for an endpoint file that is mid-write.
const ENDPOINT_RETRY: Duration = Duration::from_secs(1);

/// The polling interval while an endpoint file is mid-write.
const ENDPOINT_POLL: Duration = Duration::from_millis(25);

/// The largest welcome line accepted before the handshake is refused.
const MAX_WELCOME: usize = 64 * 1024;

/// How much partial transcript text a blocked reason quotes.
const REASON_TEXT_LIMIT: usize = 280;

/// The waiting requests, by wire id; the reader thread answers them.
type Pending = Arc<Mutex<HashMap<u64, Sender<Value>>>>;

/// Why the reader thread stopped, filled in only once on failure.
type Failure = Arc<Mutex<Option<String>>>;

/// One unsolicited push from the door, such as a `harness-event`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    frame: Value,
}

impl Event {
    /// Wrap one parsed push frame.
    fn new(frame: Value) -> Self {
        Self { frame }
    }

    /// The whole frame, exactly as the door sent it.
    pub fn frame(&self) -> &Value {
        &self.frame
    }

    /// The `event` field, such as `harness-event` or `status`.
    pub fn name(&self) -> Option<&str> {
        self.frame.get("event").and_then(Value::as_str)
    }

    /// The harness a `harness-event` belongs to.
    pub fn harness(&self) -> Option<&str> {
        self.frame.get("harness").and_then(Value::as_str)
    }

    /// The session a `harness-event` belongs to.
    pub fn session(&self) -> Option<&str> {
        self.frame.get("session").and_then(Value::as_str)
    }

    /// The `harness-event` type, such as `session-status` or `part`.
    pub fn kind(&self) -> Option<&str> {
        self.frame.get("type").and_then(Value::as_str)
    }

    /// The `data` object of a `harness-event`.
    pub fn data(&self) -> Option<&Value> {
        self.frame.get("data")
    }

    /// The `data.state` of a `session-status` event.
    pub fn state(&self) -> Option<&str> {
        self.data()
            .and_then(|data| data.get("state"))
            .and_then(Value::as_str)
    }

    /// The `data.title` of a session event.
    pub fn title(&self) -> Option<&str> {
        self.data()
            .and_then(|data| data.get("title"))
            .and_then(Value::as_str)
    }

    /// Whether this push is a `harness-event` rather than a `status` push.
    pub fn is_harness_event(&self) -> bool {
        self.name() == Some("harness-event")
    }
}

/// One authenticated NDJSON connection to warp-agent's door.
///
/// The connection is created by [`DoorClient::connect`] or
/// [`DoorClient::local`]. A single reader thread lives with the client and is
/// stopped by its `Drop`; a dead connection turns every later request into an
/// [`Error::Harness`] instead of a panic.
pub struct DoorClient {
    writer: Mutex<TcpStream>,
    pending: Pending,
    events: Receiver<Event>,
    next_id: AtomicU64,
    closed: Arc<AtomicBool>,
    failure: Failure,
    reader: Option<JoinHandle<()>>,
}

impl fmt::Debug for DoorClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DoorClient")
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl DoorClient {
    /// Connect to `127.0.0.1:port` and authenticate with `token`.
    ///
    /// The token is the only thing sent before the door's welcome: it is the
    /// first line, and its answer carries no `id`. A refused welcome is an
    /// [`Error::Harness`].
    pub fn connect(port: u16, token: &str) -> Result<Self> {
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let mut stream =
            TcpStream::connect_timeout(&address, HANDSHAKE_TIMEOUT).map_err(|error| {
                Error::Harness(format!(
                    "connecting to the door at {address} failed: {error}"
                ))
            })?;
        stream
            .set_read_timeout(Some(HANDSHAKE_TIMEOUT))
            .map_err(|error| {
                Error::Harness(format!(
                    "setting the door handshake timeout failed: {error}"
                ))
            })?;
        stream
            .set_write_timeout(Some(HANDSHAKE_TIMEOUT))
            .map_err(|error| {
                Error::Harness(format!(
                    "setting the door handshake timeout failed: {error}"
                ))
            })?;

        let auth = json!({"auth": token});
        let mut line = serde_json::to_vec(&auth)
            .map_err(|error| Error::Harness(format!("encoding the door auth failed: {error}")))?;
        line.push(b'\n');
        stream
            .write_all(&line)
            .map_err(|error| Error::Harness(format!("sending the door auth failed: {error}")))?;
        stream
            .flush()
            .map_err(|error| Error::Harness(format!("sending the door auth failed: {error}")))?;

        let welcome_line = read_limited_line(&mut stream)
            .map_err(|error| Error::Harness(format!("reading the door welcome failed: {error}")))?;
        let welcome: Value = serde_json::from_str(&welcome_line)
            .map_err(|error| Error::Harness(format!("the door welcome is not JSON: {error}")))?;
        if welcome.get("ok") != Some(&Value::Bool(true)) {
            return Err(Error::Harness(format!(
                "the door refused authentication: {welcome}"
            )));
        }
        stream.set_read_timeout(None).map_err(|error| {
            Error::Harness(format!(
                "clearing the door handshake timeout failed: {error}"
            ))
        })?;
        // The reader is allowed to block until the door speaks; a write gets
        // the same deadline as the answer it precedes, so a wedged socket
        // cannot hang a request before its `recv_timeout` even starts.
        stream
            .set_write_timeout(Some(REQUEST_TIMEOUT))
            .map_err(|error| {
                Error::Harness(format!("setting the door write timeout failed: {error}"))
            })?;

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let failure: Failure = Arc::new(Mutex::new(None));
        let closed = Arc::new(AtomicBool::new(false));
        let (event_sender, event_receiver) = mpsc::channel();
        let reader_stream = stream.try_clone().map_err(|error| {
            Error::Harness(format!("cloning the door connection failed: {error}"))
        })?;

        let thread_pending = Arc::clone(&pending);
        let thread_failure = Arc::clone(&failure);
        let thread_closed = Arc::clone(&closed);
        let reader = thread::Builder::new()
            .name("warp-flow-door".to_owned())
            .spawn(move || {
                reader_loop(
                    reader_stream,
                    thread_pending,
                    event_sender,
                    thread_closed,
                    thread_failure,
                );
            })
            .map_err(|error| Error::Harness(format!("starting the door reader failed: {error}")))?;

        Ok(Self {
            writer: Mutex::new(stream),
            pending,
            events: event_receiver,
            next_id: AtomicU64::new(0),
            closed,
            failure,
            reader: Some(reader),
        })
    }

    /// Connect to the endpoint warp-agent publishes under `~/.warp/agent`.
    ///
    /// The token is read first (`ipc.token` then `ipc.port`): warp-agent
    /// publishes the token before the port, so a port that exists implies a
    /// complete token. Both files are retried briefly, because the publisher
    /// may be caught mid-write.
    pub fn local() -> Result<Self> {
        let (port, token) = Self::local_endpoint()?;
        Self::connect(port, &token)
    }

    /// Read the published endpoint without connecting to it.
    ///
    /// Returns the port and the token. Home is resolved from `HOME` first and
    /// `USERPROFILE` second, so the same code works under a Unix shell on
    /// Windows.
    pub fn local_endpoint() -> Result<(u16, String)> {
        let home = home_dir().ok_or_else(|| {
            Error::Harness(
                "cannot find the warp-agent endpoint: neither HOME nor USERPROFILE is set"
                    .to_owned(),
            )
        })?;
        let directory = home.join(".warp").join("agent");
        let port_path = directory.join("ipc.port");
        let token_path = directory.join("ipc.token");

        let token = read_endpoint_value(&token_path)?;
        let port_text = read_endpoint_value(&port_path)?;
        let port: u16 = port_text.parse().map_err(|_| {
            Error::Harness(format!(
                "{} does not hold a TCP port: {port_text:?}",
                port_path.display()
            ))
        })?;
        if port == 0 {
            return Err(Error::Harness(format!(
                "{} holds port 0",
                port_path.display()
            )));
        }
        Ok((port, token))
    }

    /// Send one request and wait for its answer.
    ///
    /// `params` is merged into the request frame, so `models` takes
    /// `{"refresh": false}` and `harness` takes `{"method": …,"params": …}`.
    /// The answer is the whole frame, with `ok` already `true`; a refusal is
    /// an [`Error::Harness`] carrying the door's exact body. A request that is
    /// not answered within 60 seconds is also an [`Error::Harness`].
    pub fn request(&self, cmd: &str, params: Value) -> Result<Value> {
        let mut frame = match params {
            Value::Object(map) => map,
            Value::Null => Map::new(),
            other => {
                return Err(Error::Harness(format!(
                    "the params for `{cmd}` are not a JSON object: {other}"
                )));
            }
        };
        let id = self
            .next_id
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        frame.insert("id".to_owned(), Value::from(id));
        frame.insert("cmd".to_owned(), Value::from(cmd));
        let mut encoded = serde_json::to_vec(&Value::Object(frame))
            .map_err(|error| Error::Harness(format!("encoding `{cmd}` failed: {error}")))?;
        encoded.push(b'\n');

        let (answer_sender, answer_receiver) = mpsc::channel();
        {
            // Check for a dead reader and register the waiter under the same
            // lock the reader takes on exit: either the reader clears this
            // waiter (and the request wakes with `Disconnected`), or this
            // request sees `closed` and fails now instead of after 60 s.
            let mut pending = lock(&self.pending);
            if self.is_closed() {
                return Err(Error::Harness(format!(
                    "the door connection is closed: {}",
                    self.closed_reason()
                )));
            }
            pending.insert(id, answer_sender);
        }
        let written = {
            let mut writer = lock(&self.writer);
            writer.write_all(&encoded).and_then(|()| writer.flush())
        };
        if let Err(error) = written {
            lock(&self.pending).remove(&id);
            return Err(Error::Harness(format!(
                "writing `{cmd}` to the door failed: {error}"
            )));
        }

        let answer = match answer_receiver.recv_timeout(REQUEST_TIMEOUT) {
            Ok(answer) => answer,
            Err(RecvTimeoutError::Timeout) => {
                lock(&self.pending).remove(&id);
                return Err(Error::Harness(format!(
                    "the door did not answer `{cmd}` within {REQUEST_TIMEOUT:?}"
                )));
            }
            Err(RecvTimeoutError::Disconnected) => {
                lock(&self.pending).remove(&id);
                return Err(Error::Harness(format!(
                    "the door connection closed while waiting for `{cmd}`: {}",
                    self.closed_reason()
                )));
            }
        };

        if answer.get("ok") != Some(&Value::Bool(true)) {
            return Err(Error::Harness(format!(
                "the door refused `{cmd}`: {answer}"
            )));
        }
        Ok(answer)
    }

    /// Subscribe to a harness, and optionally to one session of it.
    ///
    /// The subscription id is chosen by the caller and echoed in every push
    /// the subscription matches, which is how several subscriptions on one
    /// connection are told apart.
    pub fn subscribe(
        &self,
        subscription: &str,
        harness: &str,
        session: Option<&str>,
    ) -> Result<()> {
        let mut params = json!({"subscription": subscription, "harness": harness});
        if let Some(session) = session {
            insert(&mut params, "session", session);
        }
        self.request("subscribe", params).map(|_| ())
    }

    /// Drop one subscription by its id.
    pub fn unsubscribe(&self, subscription: &str) -> Result<()> {
        self.request("unsubscribe", json!({"subscription": subscription}))
            .map(|_| ())
    }

    /// Wait up to `timeout` for the next unsolicited push.
    ///
    /// `None` means the timeout expired, the connection closed, or the reader
    /// stopped; [`DoorClient::is_closed`] tells the last two apart from the
    /// first. A closed connection never panics here.
    pub fn next_event(&self, timeout: Duration) -> Option<Event> {
        self.events.recv_timeout(timeout).ok()
    }

    /// Whether the reader thread has stopped, normally because the door
    /// closed the connection.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// Why the reader stopped, for an error that mentions a dead connection.
    fn closed_reason(&self) -> String {
        let failure = lock(&self.failure);
        match failure.as_deref() {
            Some(reason) => reason.to_owned(),
            None => "the reader stopped without a reason".to_owned(),
        }
    }
}

impl Drop for DoorClient {
    fn drop(&mut self) {
        // A shutdown wakes the blocked reader; joining it keeps a client from
        // leaking a thread after the connection is no longer wanted.
        {
            let writer = lock(&self.writer);
            // The socket may already be gone; teardown is best effort.
            let _ = writer.shutdown(Shutdown::Both);
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

/// How long [`DoorHarness`] waits for the two asynchronous halves of a step.
///
/// The defaults are the measured caps (20 s for the create push, 90 s for
/// idle); tests shrink them so a timeout is observed without waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoorTimeouts {
    /// How long a freshly created session may take to be named in a push.
    pub session: Duration,
    /// How long a prompted session may take to report `idle`.
    pub idle: Duration,
}

impl Default for DoorTimeouts {
    fn default() -> Self {
        Self {
            session: SESSION_TIMEOUT,
            idle: IDLE_TIMEOUT,
        }
    }
}

/// The [`Harness`] the app uses: one door session per workflow step.
#[derive(Debug)]
pub struct DoorHarness {
    client: DoorClient,
    timeouts: DoorTimeouts,
}

impl DoorHarness {
    /// Connect to the endpoint warp-agent publishes under `~/.warp/agent`.
    pub fn local() -> Result<Self> {
        Ok(Self::with_timeouts(
            DoorClient::local()?,
            DoorTimeouts::default(),
        ))
    }

    /// Connect to a door at `127.0.0.1:port` with default timeouts.
    pub fn connect(port: u16, token: &str) -> Result<Self> {
        Ok(Self::with_timeouts(
            DoorClient::connect(port, token)?,
            DoorTimeouts::default(),
        ))
    }

    /// Use an already connected client, with the given caps.
    pub fn with_timeouts(client: DoorClient, timeouts: DoorTimeouts) -> Self {
        Self { client, timeouts }
    }

    /// The client, for callers that need the raw door (or its liveness).
    pub fn client(&self) -> &DoorClient {
        &self.client
    }

    /// Run one step as one harness session.
    fn run_step(&self, request: &StepRequest) -> Result<StepOutcome> {
        let (harness, model) = self.select_harness()?;
        let directory = request.workdir.to_string_lossy().into_owned();

        // `harness start` is what attaches the event pump for a
        // workspace-scoped harness; it is a fast no-op for an already ready
        // managed one, and this client never opens a workspace instead.
        self.call("start", base_params(&harness, &directory))?;

        // The create reply cannot carry a session id (`id` is the reserved
        // request id and the door drops payload keys with that name), so
        // subscribe to the harness before create and learn the id from the
        // push. Pushes are live only: a subscription made afterwards misses it.
        let create_subscription = unique_name("warp-flow-create");
        self.client
            .subscribe(&create_subscription, &harness, None)?;
        let mut known: HashSet<String> = HashSet::new();
        while let Some(event) = self.client.next_event(Duration::ZERO) {
            if let Some(session) = event.session() {
                known.insert(session.to_owned());
            }
        }

        let title = session_title(&request.step.id);
        let mut create = base_params(&harness, &directory);
        insert(&mut create, "title", &title);
        insert(&mut create, "model", &model);
        if let Err(error) = self.call("create", create) {
            // The run is already failing; drop the broad subscription on a
            // best-effort basis so it does not keep feeding this connection.
            let _ = self.client.unsubscribe(&create_subscription);
            return Err(error);
        }
        let session = match self.await_session(&harness, &title, &mut known) {
            Ok(session) => session,
            Err(error) => {
                let _ = self.client.unsubscribe(&create_subscription);
                return Err(error);
            }
        };

        self.client.unsubscribe(&create_subscription)?;
        let run_subscription = unique_name("warp-flow-run");
        self.client
            .subscribe(&run_subscription, &harness, Some(&session))?;

        let mut prompt = base_params(&harness, &directory);
        insert(&mut prompt, "session", &session);
        insert(&mut prompt, "text", &request.prompt);
        insert(&mut prompt, "model", &model);
        // `ok` here is only the adapter's acceptance, not a finished turn;
        // completion is the `session-status` `idle` push below.
        self.call("send_prompt", prompt)?;

        match self.await_idle(&harness, &session)? {
            Status::Idle => {
                let text = self.transcript_text(&harness, &session, &directory)?;
                let wrote = record_session(request, &harness, &session, "done", &text)
                    .into_iter()
                    .collect();
                Ok(StepOutcome::Done { wrote, text })
            }
            Status::Failed(message) => {
                let transcript = self.transcript_text(&harness, &session, &directory);
                let recorded = match &transcript {
                    Ok(text) => text.clone(),
                    Err(error) => error.to_string(),
                };
                let reason = with_partial_text(message, transcript);
                record_session(request, &harness, &session, "failed", &recorded);
                Ok(StepOutcome::Blocked { reason })
            }
            Status::TimedOut => {
                let reason = format!(
                    "the session did not report idle within {:?}",
                    self.timeouts.idle
                );
                let mut interrupted = reason;
                let interrupt = json!({
                    "harness": harness,
                    "session": session,
                    "directory": directory,
                });
                match self.call("interrupt", interrupt) {
                    Ok(_) => interrupted.push_str("; interrupted"),
                    Err(error) => {
                        interrupted.push_str(&format!("; interrupt failed: {error}"));
                    }
                }
                let transcript = self.transcript_text(&harness, &session, &directory);
                let recorded = match &transcript {
                    Ok(text) => text.clone(),
                    Err(error) => error.to_string(),
                };
                let reason = with_partial_text(interrupted, transcript);
                record_session(request, &harness, &session, "timed-out", &recorded);
                Ok(StepOutcome::Blocked { reason })
            }
        }
    }

    /// Pick an enabled, available harness and the first model of its catalog.
    ///
    /// A managed connection wins over an attached one (attached adapters have
    /// no event pump through the door), and `dsh` is preferred among equals.
    /// Nothing is hardcoded to `opencode`: the v1 harness is workspace-scoped
    /// and often disabled. The model is looked up only after the harness has
    /// been chosen, from the cached catalog (`refresh: false`).
    fn select_harness(&self) -> Result<(String, String)> {
        let metadata = self
            .client
            .request("harness", json!({"method": "metadata", "params": {}}))?;
        let published = metadata
            .get("harnesses")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::Harness("`harness metadata` answered without a `harnesses` array".to_owned())
            })?;

        let candidates: Vec<&Value> = published
            .iter()
            .filter(|entry| {
                entry.get("enabled") == Some(&Value::Bool(true))
                    && entry.get("available") == Some(&Value::Bool(true))
                    && entry.get("id").and_then(Value::as_str).is_some()
            })
            .collect();
        if candidates.is_empty() {
            return Err(Error::Harness(
                "the door publishes no enabled, available harness".to_owned(),
            ));
        }

        let models = self.client.request("models", json!({"refresh": false}))?;
        let catalog = models
            .get("catalog")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let model_for = |id: &str| -> Option<String> {
            catalog
                .iter()
                .find(|entry| entry.get("harness").and_then(Value::as_str) == Some(id))
                .and_then(|entry| entry.get("key"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };

        let mut ranked: Vec<((bool, bool, String), &Value)> = candidates
            .into_iter()
            .map(|entry| {
                let id = entry.get("id").and_then(Value::as_str).unwrap_or_default();
                let managed = entry.get("connection").and_then(Value::as_str) == Some("managed");
                let rank = (!managed, id != "dsh", id.to_owned());
                (rank, entry)
            })
            .collect();
        ranked.sort_by(|left, right| left.0.cmp(&right.0));

        let chosen = ranked
            .first()
            .map(|(_, entry)| *entry)
            .ok_or_else(|| Error::Harness("no selectable harness was reported".to_owned()))?;
        let id = chosen
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let model = model_for(&id).ok_or_else(|| {
            Error::Harness(format!("the door publishes no model for harness `{id}`"))
        })?;
        Ok((id, model))
    }

    /// Wait for the push that names the session `harness create` just made.
    fn await_session(
        &self,
        harness: &str,
        title: &str,
        known: &mut HashSet<String>,
    ) -> Result<String> {
        let deadline = Instant::now() + self.timeouts.session;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let Some(event) = self.client.next_event(remaining) else {
                if self.client.is_closed() {
                    return Err(Error::Harness(format!(
                        "the door connection closed before the new session was named: {}",
                        self.client.closed_reason()
                    )));
                }
                break;
            };
            if !event.is_harness_event() || event.harness() != Some(harness) {
                continue;
            }
            let Some(session) = event.session() else {
                continue;
            };
            if event.title() == Some(title) && !known.contains(session) {
                return Ok(session.to_owned());
            }
            known.insert(session.to_owned());
        }
        Err(Error::Harness(format!(
            "`harness create` was acknowledged but no push named the new session `{title}` within {:?}",
            self.timeouts.session
        )))
    }

    /// Wait until the session reports `idle`, an error arrives, or the cap
    /// expires. Identical frames are ignored: the backend broadcasts some
    /// statuses twice, and a duplicate `busy` must not extend the wait.
    fn await_idle(&self, harness: &str, session: &str) -> Result<Status> {
        let deadline = Instant::now() + self.timeouts.idle;
        let mut seen: HashSet<String> = HashSet::new();
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(Status::TimedOut);
            }
            let Some(event) = self.client.next_event(remaining) else {
                if self.client.is_closed() {
                    return Err(Error::Harness(format!(
                        "the door connection closed while waiting for session `{session}`: {}",
                        self.client.closed_reason()
                    )));
                }
                return Ok(Status::TimedOut);
            };
            if !event.is_harness_event()
                || event.harness() != Some(harness)
                || event.session() != Some(session)
            {
                continue;
            }
            if !seen.insert(fingerprint(&event)) {
                continue;
            }
            match event.kind() {
                Some("session-status") => match event.state() {
                    Some("idle") => return Ok(Status::Idle),
                    Some("error") => {
                        return Ok(Status::Failed(format!(
                            "the harness reported session status `error`{}",
                            message_suffix(&event)
                        )));
                    }
                    _ => {}
                },
                Some("error") => {
                    return Ok(Status::Failed(format!(
                        "the harness reported an error{}",
                        message_suffix(&event)
                    )));
                }
                _ => {}
            }
        }
    }

    /// Fetch the transcript and return the last assistant message's text.
    ///
    /// The transcript is authoritative: the user `part` stream repeats the
    /// prompt, and `part` frames in the event stream are full snapshots of one
    /// part, not deltas. Parts of one transcript message are distinct content,
    /// so they are joined in order.
    fn transcript_text(&self, harness: &str, session: &str, directory: &str) -> Result<String> {
        let answer = self.call(
            "transcript",
            json!({"harness": harness, "session": session, "directory": directory}),
        )?;
        let messages = answer
            .get("messages")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                Error::Harness(
                    "`harness transcript` answered without a `messages` array".to_owned(),
                )
            })?;
        let Some(message) = messages
            .iter()
            .rev()
            .find(|entry| message_role(entry) == Some("assistant"))
        else {
            return Ok(String::new());
        };
        let mut text = String::new();
        let Some(parts) = message.get("parts").and_then(Value::as_array) else {
            return Ok(text);
        };
        for part in parts {
            let Some(payload) = part.get("payload") else {
                continue;
            };
            if payload.get("kind").and_then(Value::as_str) == Some("text")
                && let Some(chunk) = payload.get("text").and_then(Value::as_str)
            {
                text.push_str(chunk);
            }
        }
        Ok(text)
    }

    /// One `harness` method call.
    fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.client
            .request("harness", json!({"method": method, "params": params}))
    }
}

impl Harness for DoorHarness {
    fn run(&mut self, request: &StepRequest) -> Result<StepOutcome> {
        self.run_step(request)
    }
}

/// How an idle wait ended.
enum Status {
    /// The session reported `idle`.
    Idle,
    /// The harness reported an error instead of finishing.
    Failed(String),
    /// The cap expired while the session was still working.
    TimedOut,
}

/// The reader thread: route answers by id and pushes onto the event channel.
fn reader_loop(
    stream: TcpStream,
    pending: Pending,
    events: Sender<Event>,
    closed: Arc<AtomicBool>,
    failure: Failure,
) {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                set_failure(&failure, "the door closed the connection");
                break;
            }
            Ok(_) => {
                let text = line.trim();
                if text.is_empty() {
                    continue;
                }
                match serde_json::from_str::<Value>(text) {
                    Ok(frame) => route(frame, &pending, &events),
                    Err(error) => {
                        set_failure(
                            &failure,
                            &format!("the door sent a frame that is not JSON: {error}"),
                        );
                        break;
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => {
                set_failure(&failure, &format!("reading from the door failed: {error}"));
                break;
            }
        }
    }
    // Set `closed` and drop the waiting senders under the pending lock, so a
    // request that registers at the same time cannot be missed: it sees
    // `closed`, or its sender is cleared here and it wakes with `Disconnected`.
    // Both beat leaving it to the 60 s deadline.
    {
        let mut pending = lock(&pending);
        closed.store(true, Ordering::SeqCst);
        pending.clear();
    }
}

/// Route one parsed frame: pushes to the event channel, answers to their id.
fn route(frame: Value, pending: &Pending, events: &Sender<Event>) {
    if frame.get("event").is_some() {
        let _ = events.send(Event::new(frame));
        return;
    }
    let Some(id) = frame.get("id").and_then(Value::as_u64) else {
        return;
    };
    let sender = lock(pending).remove(&id);
    if let Some(sender) = sender {
        let _ = sender.send(frame);
    }
}

/// Record why the reader stopped, keeping the first reason.
fn set_failure(failure: &Failure, reason: &str) {
    let mut slot = lock(failure);
    if slot.is_none() {
        *slot = Some(reason.to_owned());
    }
}

/// One step attempt's session, as a file in the issue folder.
///
/// The run screen reads these to show what the harness was asked and answered:
/// the door is the only place that knows a session id, and this is where it
/// becomes markdown a person (and the engine's own record) can read. One file
/// per step and task, rewritten on every attempt — a session replaces its
/// previous attempt, it never appends to it, so the file always describes the
/// session `run.md`'s newest row names.
///
/// The write is best effort: a run must not fail because the transcript of a
/// finished session could not be stored.
fn record_session(
    request: &StepRequest,
    harness: &str,
    session: &str,
    status: &str,
    text: &str,
) -> Option<PathBuf> {
    let path = session_path(request);
    let mut front = FrontMatter::new();
    front.set("session", session);
    front.set("harness", harness);
    front.set("step", request.step.id.clone());
    if let Some(task) = request.task {
        front.set("task", format!("{task:02}"));
    }
    front.set("attempt", request.attempt.to_string());
    front.set("status", status);
    front.set("updated", timestamp(SystemTime::now()));

    let mut body = format!("# Session: {}\n\n", request.step.id);
    let text = text.trim();
    if text.is_empty() {
        body.push_str("_The session reported no text._\n");
    } else {
        body.push_str(text);
        body.push('\n');
    }
    Document::new(front, body)
        .write_atomic(&path)
        .ok()
        .map(|()| path)
}

/// Where a session file lives: `sessions/<step>.md`, or
/// `sessions/<step>-task-NN.md` for a per-task step.
fn session_path(request: &StepRequest) -> PathBuf {
    let name = match request.task {
        Some(task) => format!("{}-task-{task:02}.md", request.step.id),
        None => format!("{}.md", request.step.id),
    };
    request.issue.join("sessions").join(name)
}

#[cfg(test)]
mod session_tests {
    use super::*;
    use crate::graph::StepKind;

    fn request(task: Option<u32>) -> StepRequest {
        let dir =
            std::env::temp_dir().join(format!("warp-flow-session-{}-{task:?}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("issue folder");
        StepRequest {
            step: crate::graph::Step {
                id: "code".to_owned(),
                kind: StepKind::Harness,
                skill: Some("code".to_owned()),
                gate: None,
                depends_on: Vec::new(),
                produces: None,
                per_task: task.is_some(),
            },
            issue: dir.clone(),
            workdir: dir,
            prompt: "do it".to_owned(),
            task,
            attempt: 1,
        }
    }

    #[test]
    fn a_session_becomes_markdown_in_the_issue_folder() {
        let request = request(Some(2));
        let path = record_session(
            &request,
            "opencode",
            "ses_abc",
            "done",
            "I added the offset.",
        )
        .expect("the session is written");
        assert_eq!(
            path,
            request.issue.join("sessions/code-task-02.md"),
            "one file per step and task"
        );
        let document = Document::read(&path).expect("it parses");
        assert_eq!(document.front.get("session"), Some("ses_abc"));
        assert_eq!(document.front.get("harness"), Some("opencode"));
        assert_eq!(document.front.get("step"), Some("code"));
        assert_eq!(document.front.get("task"), Some("02"));
        assert_eq!(document.front.get("attempt"), Some("1"));
        assert_eq!(document.front.get("status"), Some("done"));
        assert!(document.body.contains("I added the offset."));
    }

    #[test]
    fn a_second_attempt_replaces_the_first_session_file() {
        let mut request = request(None);
        request.attempt = 1;
        let first = record_session(&request, "opencode", "ses_one", "failed", "first try")
            .expect("the first attempt writes");
        request.attempt = 2;
        let second = record_session(&request, "opencode", "ses_two", "done", "second try")
            .expect("the second attempt writes");
        assert_eq!(first, second, "the same step replaces its own session file");
        let text = std::fs::read_to_string(&second).expect("the file");
        assert!(
            text.contains("ses_two"),
            "the newest session is the one kept"
        );
        assert!(!text.contains("ses_one"));
        assert!(text.contains("second try"));
        let _ = std::fs::remove_dir_all(&request.issue);
    }
}

/// The line the door's welcome arrives on; small enough to read byte by byte
/// so no buffered bytes are lost when the reader thread takes the stream over.
fn read_limited_line(stream: &mut TcpStream) -> io::Result<String> {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the door closed during the handshake",
                ));
            }
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                if byte[0] != b'\r' {
                    bytes.push(byte[0]);
                }
                if bytes.len() > MAX_WELCOME {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the door welcome is too long",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    String::from_utf8(bytes).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Read one endpoint file, retrying briefly while the publisher writes it.
fn read_endpoint_value(path: &Path) -> Result<String> {
    let deadline = Instant::now() + ENDPOINT_RETRY;
    loop {
        match fs::read(path) {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => {
                    let text = text.trim();
                    if !text.is_empty() {
                        return Ok(text.to_owned());
                    }
                }
                Err(_) => {
                    return Err(Error::Harness(format!("{} is not UTF-8", path.display())));
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(Error::io(path, error)),
        }
        if Instant::now() >= deadline {
            return Err(Error::Harness(format!(
                "the door is not published: {} is missing or empty",
                path.display()
            )));
        }
        thread::sleep(ENDPOINT_POLL);
    }
}

/// The home directory: `HOME` first, `USERPROFILE` second.
fn home_dir() -> Option<PathBuf> {
    ["HOME", "USERPROFILE"].iter().find_map(|key| {
        std::env::var_os(key)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
    })
}

/// Insert a string field into a params object.
fn insert(params: &mut Value, key: &str, value: &str) {
    if let Value::Object(map) = params {
        map.insert(key.to_owned(), Value::from(value));
    }
}

/// The params every `harness` method that starts or addresses a session needs.
fn base_params(harness: &str, directory: &str) -> Value {
    json!({"harness": harness, "directory": directory})
}

/// A title unique enough to identify the session on a busy harness.
fn session_title(step: &str) -> String {
    let token = unique_token();
    format!("warp-flow {step} {token}")
}

/// A name unique within the process and across runs.
fn unique_name(prefix: &str) -> String {
    let token = unique_token();
    format!("{prefix}-{token}")
}

/// Process id, wall time and a counter, so two runs in one process differ.
fn unique_token() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
    format!("{}-{millis}-{ordinal}", std::process::id())
}

/// A dedupe key that ignores the `subscription` field: two subscriptions may
/// deliver the same event, and the backend itself broadcasts some statuses
/// twice with a byte-identical body.
fn fingerprint(event: &Event) -> String {
    json!({
        "harness": event.harness(),
        "session": event.session(),
        "type": event.kind(),
        "data": event.data(),
    })
    .to_string()
}

/// The `data.message` of an error event, as `: message` or empty.
fn message_suffix(event: &Event) -> String {
    let message = event
        .data()
        .and_then(|data| data.get("message"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if message.is_empty() {
        String::new()
    } else {
        format!(": {message}")
    }
}

/// Append a bounded excerpt of the partial transcript, if there is one.
fn with_partial_text(reason: String, transcript: Result<String>) -> String {
    let mut reason = reason;
    match transcript {
        Ok(text) => {
            let text = text.trim();
            if !text.is_empty() {
                let excerpt: String = text.chars().take(REASON_TEXT_LIMIT).collect();
                reason.push_str(&format!("; partial transcript: {excerpt}"));
                if text.chars().count() > REASON_TEXT_LIMIT {
                    reason.push_str("...");
                }
            }
        }
        Err(error) => reason.push_str(&format!("; transcript unavailable: {error}")),
    }
    reason
}

/// The role of one transcript entry.
fn message_role(entry: &Value) -> Option<&str> {
    entry.get("message")?.get("role")?.as_str()
}

/// Lock a mutex, recovering the data if another thread panicked while holding
/// it. The engine must not take the process down because a peer thread died.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_transcript_is_quoted_and_bounded() {
        let long = "x".repeat(REASON_TEXT_LIMIT + 10);
        let reason = with_partial_text("timed out".to_owned(), Ok(long));
        assert!(reason.contains("timed out"));
        assert!(reason.contains("partial transcript"));
        assert!(reason.ends_with("..."));
    }

    #[test]
    fn a_failed_transcript_is_reported_in_the_reason() {
        let reason = with_partial_text(
            "timed out".to_owned(),
            Err(Error::Harness("no answer".to_owned())),
        );
        assert!(reason.contains("transcript unavailable"));
        assert!(reason.contains("no answer"));
    }

    #[test]
    fn the_dedupe_key_ignores_the_subscription() {
        let first = Event::new(json!({
            "event": "harness-event",
            "harness": "dsh",
            "session": "s",
            "type": "session-status",
            "data": {"state": "busy"},
            "subscription": "one",
        }));
        let second = Event::new(json!({
            "event": "harness-event",
            "harness": "dsh",
            "session": "s",
            "type": "session-status",
            "data": {"state": "busy"},
            "subscription": "two",
        }));
        assert_eq!(fingerprint(&first), fingerprint(&second));
    }
}
