//! Probe: the door client and the [`Harness`] over it against an in-process
//! fake door.
//!
//! The fake speaks the frames measured in
//! `.scratch/warp-flow/spike/FINDINGS.md`: auth, `ping`, `harness metadata`,
//! `models`, `harness start`, `subscribe`, `harness create` (which pushes
//! `session-changed` before its answer, exactly as the real door does),
//! `send_prompt` (which pushes `busy`, the user message and part, the
//! assistant message and part, and `idle` — twice, because the backend
//! duplicates status broadcasts), `harness interrupt`, and
//! `harness transcript`. Every frame the client sends is recorded, so the
//! probe can assert the order and the exact params.
//!
//! There is no network: the listener is on `127.0.0.1:0`, the port is read
//! from its own address, and the server thread dies with the fake.

use std::io::{BufRead, BufReader, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};
use warp_flow_core::door::{DoorClient, DoorHarness, DoorTimeouts};
use warp_flow_core::graph::{Step, StepKind};
use warp_flow_core::harness::{Harness, StepOutcome, StepRequest};

const TOKEN: &str = "probe-token";
const SESSION: &str = "session-fef8a56b-968c-4b7d-a624-169c0d322ee9";
const HARNESS: &str = "dsh";
const MODEL: &str = "opencode-go/deepseek-v4.1-flash";
const PROMPT: &str = "Reply with exactly the word ok and nothing else.";

/// How long a streaming turn keeps moving, and how often a snapshot lands.
///
/// The total (`STREAM_FRAMES * STREAM_INTERVAL`) is deliberately longer than
/// the idle cap the probe hands the client, so a client that measures the
/// whole turn rather than the silence gives up on a session that is working.
const STREAM_FRAMES: u32 = 20;
const STREAM_INTERVAL: Duration = Duration::from_millis(50);

/// How the fake door ends a prompt.
#[derive(Debug, Clone, Copy)]
enum Mode {
    /// Answer with `ok` and finish with `idle` (and duplicate statuses).
    RoundTrip,
    /// Push everything but never `idle`, so the client's cap expires.
    NeverIdle,
    /// Keep streaming part snapshots for longer than the cap, then `idle`:
    /// a working session, not a stuck one.
    Streaming,
    /// Close the connection right after the welcome.
    CloseAfterAuth,
}

/// One request the fake received, in order.
#[derive(Debug, Clone)]
struct Recorded {
    cmd: String,
    method: Option<String>,
    frame: Value,
}

impl Recorded {
    fn params(&self) -> &Value {
        static NULL: Value = Value::Null;
        self.frame.get("params").unwrap_or(&NULL)
    }
}

/// In-process NDJSON door on a kernel-assigned loopback port.
struct FakeDoor {
    port: u16,
    records: Arc<Mutex<Vec<Recorded>>>,
    duplicates: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeDoor {
    fn start(mode: Mode) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind the fake door");
        let port = listener
            .local_addr()
            .expect("the fake door has an address")
            .port();
        listener.set_nonblocking(true).expect("a pollable listener");
        let records = Arc::new(Mutex::new(Vec::new()));
        let duplicates = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));

        let thread_records = Arc::clone(&records);
        let thread_duplicates = Arc::clone(&duplicates);
        let thread_stop = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("fake-door".to_owned())
            .spawn(move || {
                while !thread_stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            if stream.set_nonblocking(false).is_ok() {
                                serve(stream, &thread_records, &thread_duplicates, mode);
                            }
                            return;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(2));
                        }
                        Err(_) => return,
                    }
                }
            })
            .expect("start the fake door thread");

        Self {
            port,
            records,
            duplicates,
            stop,
            handle: Some(handle),
        }
    }

    fn port(&self) -> u16 {
        self.port
    }

    fn records(&self) -> Vec<Recorded> {
        lock(&self.records).clone()
    }

    fn duplicates(&self) -> usize {
        self.duplicates.load(Ordering::SeqCst)
    }
}

impl Drop for FakeDoor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// One connection: auth, then answer or push for every request until EOF.
fn serve(
    stream: TcpStream,
    records: &Arc<Mutex<Vec<Recorded>>>,
    duplicates: &Arc<AtomicUsize>,
    mode: Mode,
) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let authenticated = serde_json::from_str::<Value>(line.trim())
        .map(|frame| frame.get("auth").and_then(Value::as_str) == Some(TOKEN))
        .unwrap_or(false);
    if send(&mut writer, &json!({"ok": authenticated})).is_err() || !authenticated {
        return;
    }
    if matches!(mode, Mode::CloseAfterAuth) {
        let _ = writer.shutdown(Shutdown::Both);
        return;
    }

    let mut subscription: Option<String> = None;
    let mut prompt = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => return,
            Ok(_) => {}
            Err(_) => return,
        }
        let Ok(frame) = serde_json::from_str::<Value>(line.trim()) else {
            return;
        };
        let id = frame.get("id").cloned().unwrap_or(Value::Null);
        let cmd = frame
            .get("cmd")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        lock(records).push(Recorded {
            cmd: cmd.clone(),
            method: method.clone(),
            frame: frame.clone(),
        });

        let result = match (cmd.as_str(), method.as_deref()) {
            ("ping", _) => send(
                &mut writer,
                &json!({"id": id, "ok": true, "version": 3, "app": "warp-agent"}),
            ),
            ("harness", Some("metadata")) => send(
                &mut writer,
                &json!({"id": id, "ok": true, "harnesses": harness_metadata()}),
            ),
            ("models", _) => send(
                &mut writer,
                &json!({
                    "id": id,
                    "ok": true,
                    "models": ["opencode-v2/free-model", MODEL],
                    "catalog": catalog(),
                    "harnesses": [{"id": "opencode-v2", "label": "opencode v2"}, {"id": HARNESS, "label": "DeepSeek (dsh)"}],
                }),
            ),
            ("harness", Some("start")) => send(&mut writer, &json!({"id": id, "ok": true})),
            ("subscribe", _) => {
                subscription = frame
                    .get("subscription")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                send(
                    &mut writer,
                    &json!({
                        "id": id,
                        "ok": true,
                        "subscription": frame.get("subscription").cloned().unwrap_or(Value::Null),
                    }),
                )
            }
            ("unsubscribe", _) => {
                if subscription.as_deref() == frame.get("subscription").and_then(Value::as_str) {
                    subscription = None;
                }
                send(
                    &mut writer,
                    &json!({
                        "id": id,
                        "ok": true,
                        "subscription": frame.get("subscription").cloned().unwrap_or(Value::Null),
                        "unsubscribed": true,
                    }),
                )
            }
            ("harness", Some("create")) => {
                create(&mut writer, &id, &params, subscription.as_deref())
            }
            ("harness", Some("send_prompt")) => {
                prompt = params
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                send(&mut writer, &json!({"id": id, "ok": true})).and_then(|()| {
                    pushes(
                        &mut writer,
                        mode,
                        subscription.as_deref(),
                        &prompt,
                        duplicates,
                    )
                })
            }
            ("harness", Some("transcript")) => {
                let text = match mode {
                    Mode::NeverIdle => "partial",
                    Mode::Streaming => "streamed",
                    _ => "ok",
                };
                send(&mut writer, &transcript_answer(&id, &prompt, text))
            }
            ("harness", Some("interrupt")) => send(&mut writer, &json!({"id": id, "ok": true})),
            ("open-workspace", _) => send(
                &mut writer,
                &json!({"id": id, "ok": false, "error": "the probe forbids open-workspace"}),
            ),
            _ => send(
                &mut writer,
                &json!({"id": id, "ok": false, "error": format!("unknown command `{cmd}`")}),
            ),
        };
        if result.is_err() {
            return;
        }
    }
}

/// The enabled harness set: `dsh` is managed; `opencode-v2` is attached; the
/// disabled and the unavailable entries must not be selected.
fn harness_metadata() -> Value {
    json!([
        {
            "id": "opencode-v2",
            "label": "opencode v2",
            "connection": "attached",
            "enabled": true,
            "available": true,
            "state": "ready",
        },
        {
            "id": HARNESS,
            "label": "DeepSeek (dsh)",
            "connection": "managed",
            "enabled": true,
            "available": true,
            "state": "ready",
        },
        {
            "id": "opencode",
            "label": "opencode",
            "connection": "managed",
            "enabled": false,
            "available": true,
            "state": "stopped",
        },
        {
            "id": "cold",
            "label": "cold managed",
            "connection": "managed",
            "enabled": true,
            "available": false,
            "state": "stopped",
        },
    ])
}

/// The measured catalog shape, one entry per harness.
fn catalog() -> Value {
    json!([
        {
            "key": "opencode-v2/free-model",
            "harness": "opencode-v2",
            "variants": [],
            "context_limit": 100_000,
        },
        {
            "key": MODEL,
            "harness": HARNESS,
            "variants": ["low", "high", "max"],
            "context_limit": 0,
        },
    ])
}

/// `harness create`: the push naming the session arrives before the answer,
/// and the answer's `id` is the request id, never the session id.
fn create(
    stream: &mut TcpStream,
    id: &Value,
    params: &Value,
    subscription: Option<&str>,
) -> std::io::Result<()> {
    let title = params.get("title").cloned().unwrap_or(Value::Null);
    let directory = params.get("directory").cloned().unwrap_or(Value::Null);
    push_event(
        stream,
        "session-changed",
        json!({"title": title.clone(), "parent": null, "agent": null}),
        subscription.unwrap_or_default(),
    )?;
    send(
        stream,
        &json!({
            "id": id,
            "ok": true,
            "title": title,
            "parent": null,
            "directory": directory,
            "updated_ms": 1_795_271_546_014_u64,
            "created_ms": 1_795_271_546_014_u64,
        }),
    )
}

/// The recorded prompt turn, including the two duplicated status frames.
fn pushes(
    stream: &mut TcpStream,
    mode: Mode,
    subscription: Option<&str>,
    prompt: &str,
    duplicates: &AtomicUsize,
) -> std::io::Result<()> {
    let subscription = subscription.unwrap_or_default();
    push_event(
        stream,
        "session-status",
        json!({"state": "busy"}),
        subscription,
    )?;
    duplicates.fetch_add(1, Ordering::SeqCst);
    push_event(
        stream,
        "session-status",
        json!({"state": "busy"}),
        subscription,
    )?;
    push_event(
        stream,
        "message",
        json!({"id": "msg-user", "role": "user"}),
        subscription,
    )?;
    push_event(
        stream,
        "part",
        json!({
            "id": "msg-user-0",
            "message_id": "msg-user",
            "payload": {"kind": "text", "text": prompt},
        }),
        subscription,
    )?;
    push_event(
        stream,
        "message",
        json!({"id": "dsh-1-1", "role": "assistant"}),
        subscription,
    )?;
    let text = match mode {
        Mode::NeverIdle => "partial",
        Mode::Streaming => "streamed",
        _ => "ok",
    };
    push_event(
        stream,
        "part",
        json!({
            "id": "dsh-1-1-t0",
            "message_id": "dsh-1-1",
            "payload": {"kind": "text", "text": text},
        }),
        subscription,
    )?;
    if matches!(mode, Mode::Streaming) {
        // A working session keeps the part snapshot moving. The snapshots are
        // all distinct, so the client cannot dedupe them away, and the turn
        // outlives the cap it is measured against.
        for step in 1..=STREAM_FRAMES {
            thread::sleep(STREAM_INTERVAL);
            push_event(
                stream,
                "part",
                json!({
                    "id": "dsh-1-1-t0",
                    "message_id": "dsh-1-1",
                    "payload": {"kind": "text", "text": format!("{text} {step}")},
                }),
                subscription,
            )?;
        }
    }
    if matches!(mode, Mode::RoundTrip) {
        push_event(
            stream,
            "session-status",
            json!({"state": "idle"}),
            subscription,
        )?;
        duplicates.fetch_add(1, Ordering::SeqCst);
        push_event(
            stream,
            "session-status",
            json!({"state": "idle"}),
            subscription,
        )?;
    }
    if matches!(mode, Mode::Streaming) {
        push_event(
            stream,
            "session-status",
            json!({"state": "idle"}),
            subscription,
        )?;
    }
    Ok(())
}

/// The measured transcript: a user message that repeats the prompt, then the
/// assistant message that is the answer.
fn transcript_answer(id: &Value, prompt: &str, assistant: &str) -> Value {
    json!({
        "id": id,
        "ok": true,
        "messages": [
            {
                "message": {
                    "id": "msg-user",
                    "session": SESSION,
                    "role": "user",
                    "created_ms": 1_795_271_546_165_u64,
                    "completed_ms": null,
                    "provider": null,
                    "model": null,
                    "error": null,
                    "tokens": null,
                    "cost_micros": null,
                },
                "parts": [
                    {"id": "msg-user-0", "payload": {"kind": "text", "text": prompt}}
                ],
            },
            {
                "message": {
                    "id": "dsh-1-1",
                    "session": SESSION,
                    "role": "assistant",
                    "created_ms": 1_795_271_546_081_u64,
                    "completed_ms": 1_795_271_550_185_u64,
                    "provider": "opencode-go",
                    "model": "deepseek-v4.1-flash",
                    "error": null,
                    "tokens": {"input": 20070, "output": 3, "reasoning": 0, "cache_read": 0, "cache_write": 0},
                    "cost_micros": 0,
                },
                "parts": [
                    {"id": "dsh-1-1-t0", "payload": {"kind": "text", "text": assistant}}
                ],
            },
        ],
    })
}

fn push_event(
    stream: &mut TcpStream,
    kind: &str,
    data: Value,
    subscription: &str,
) -> std::io::Result<()> {
    send(
        stream,
        &json!({
            "event": "harness-event",
            "harness": HARNESS,
            "type": kind,
            "session": SESSION,
            "data": data,
            "subscription": subscription,
        }),
    )
}

fn send(stream: &mut TcpStream, frame: &Value) -> std::io::Result<()> {
    let mut line = serde_json::to_string(frame).map_err(std::io::Error::other)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    stream.flush()
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One harness step request for a fixed work directory.
fn step_request(prompt: &str) -> StepRequest {
    let workdir = std::env::temp_dir().join("warp-flow-door-probe");
    StepRequest {
        step: Step {
            id: "code".to_owned(),
            kind: StepKind::Harness,
            skill: Some("code".to_owned()),
            enabled: true,
            harness: None,
            model: None,
            effort: None,
            command: None,
            cwd: None,
            timeout_secs: None,
            depends_on: Some(Vec::new()),
            produces: None,
            edges: Vec::new(),
            per_task: false,
            mode: None,
            push: false,
            on_conflict: None,
            on_gate_failed: None,
        },
        issue: workdir.join("issue"),
        workdir,
        prompt: prompt.to_owned(),
        task: None,
        attempt: 1,
    }
}

#[test]
fn a_round_trip_returns_the_assistant_text_only() {
    let fake = FakeDoor::start(Mode::RoundTrip);
    let mut harness = DoorHarness::connect(fake.port(), TOKEN).expect("connect to the fake door");

    let outcome = harness
        .run(&step_request(PROMPT))
        .expect("the fake door completes the step");

    match outcome {
        StepOutcome::Done { wrote, text } => {
            // the one file a door step reports is the session it recorded
            assert_eq!(
                wrote.len(),
                1,
                "a door step reports its session file: {wrote:?}"
            );
            assert!(
                wrote[0].ends_with("sessions/code.md"),
                "the session file is named for the step: {wrote:?}"
            );
            assert_eq!(text, "ok", "the transcript's assistant text is the answer");
            assert_ne!(text, PROMPT, "the echoed prompt must not be returned");
        }
        other => panic!("expected Done, got {other:?}"),
    }
}

#[test]
fn duplicate_status_frames_do_not_break_the_idle_wait() {
    let fake = FakeDoor::start(Mode::RoundTrip);
    let mut harness = DoorHarness::connect(fake.port(), TOKEN).expect("connect to the fake door");

    let outcome = harness
        .run(&step_request(PROMPT))
        .expect("duplicates must not disturb the wait");

    assert!(
        matches!(outcome, StepOutcome::Done { .. }),
        "expected Done, got {outcome:?}"
    );
    assert!(
        fake.duplicates() >= 2,
        "the probe must have sent duplicate busy and idle frames, sent {}",
        fake.duplicates()
    );
}

#[test]
fn the_harness_subscribes_before_create_and_narrows_afterwards() {
    let fake = FakeDoor::start(Mode::RoundTrip);
    let mut harness = DoorHarness::connect(fake.port(), TOKEN).expect("connect to the fake door");
    let request = step_request(PROMPT);
    harness
        .run(&request)
        .expect("the fake door completes the step");
    let records = fake.records();

    let start = records
        .iter()
        .position(|record| record.cmd == "harness" && record.method.as_deref() == Some("start"))
        .expect("harness start was sent");
    let broad = records
        .iter()
        .position(|record| record.cmd == "subscribe" && record.frame.get("session").is_none())
        .expect("a harness-only subscribe was sent");
    let create = records
        .iter()
        .position(|record| record.cmd == "harness" && record.method.as_deref() == Some("create"))
        .expect("harness create was sent");
    let narrow = records
        .iter()
        .position(|record| record.cmd == "subscribe" && record.frame.get("session").is_some())
        .expect("the subscription was narrowed to the session");

    assert!(
        start < broad,
        "`harness start` must attach the pump before subscribing: {records:#?}"
    );
    assert!(
        broad < create,
        "the session id only exists in a push, so subscribe before create: {records:#?}"
    );
    assert!(
        create < narrow,
        "the narrowed subscription needs the id the push named: {records:#?}"
    );

    let created = &records[create];
    assert_eq!(created.params()["harness"], json!(HARNESS));
    assert_eq!(
        created.params()["model"],
        json!(MODEL),
        "the managed dsh harness's first catalog model is used"
    );
    assert_eq!(
        created.params()["directory"],
        json!(request.workdir.to_string_lossy().into_owned())
    );
    let prompt = records
        .iter()
        .position(|record| {
            record.cmd == "harness" && record.method.as_deref() == Some("send_prompt")
        })
        .expect("harness send_prompt was sent");
    assert_eq!(records[prompt].params()["text"], json!(PROMPT));
    assert_eq!(records[prompt].params()["model"], json!(MODEL));
    assert_eq!(records[prompt].params()["session"], json!(SESSION));
    assert_eq!(
        records[narrow].frame["session"],
        json!(SESSION),
        "the narrowed subscription targets the pushed session"
    );

    assert!(
        !records.iter().any(|record| record.cmd == "open-workspace"),
        "the door route must not create a visible workspace row"
    );
    assert!(
        !records.iter().any(|record| {
            record.cmd == "run-prompt" || record.method.as_deref() == Some("run-prompt")
        }),
        "the door route must not use run-prompt"
    );
}

#[test]
fn a_session_that_keeps_streaming_past_the_cap_finishes() {
    let fake = FakeDoor::start(Mode::Streaming);
    let client = DoorClient::connect(fake.port(), TOKEN).expect("connect to the fake door");
    // The turn outlives the silence window several times over while every
    // single gap inside it stays far below it.
    let timeouts = DoorTimeouts {
        session: Duration::from_secs(2),
        idle: Duration::from_millis(400),
        turn: Duration::from_secs(30),
    };
    let mut harness = DoorHarness::with_timeouts(client, timeouts);

    let outcome = harness
        .run(&step_request(PROMPT))
        .expect("a working session is not an engine error");

    match outcome {
        StepOutcome::Done { text, .. } => assert_eq!(
            text, "streamed",
            "the streamed turn finished and its transcript is the answer"
        ),
        StepOutcome::Blocked { reason } => {
            panic!("a session that kept pushing part snapshots was treated as stuck: {reason}")
        }
    }

    let records = fake.records();
    assert!(
        records
            .iter()
            .all(|record| record.method.as_deref() != Some("interrupt")),
        "a working session must not be interrupted: {records:#?}"
    );
}

#[test]
fn a_session_that_never_idles_is_interrupted_and_reported_blocked() {
    let fake = FakeDoor::start(Mode::NeverIdle);
    let client = DoorClient::connect(fake.port(), TOKEN).expect("connect to the fake door");
    let timeouts = DoorTimeouts {
        session: Duration::from_secs(2),
        idle: Duration::from_millis(250),
        turn: Duration::from_secs(30),
    };
    let mut harness = DoorHarness::with_timeouts(client, timeouts);

    let outcome = harness
        .run(&step_request(PROMPT))
        .expect("an idle timeout is a blocked step, not an engine error");

    match outcome {
        StepOutcome::Blocked { reason } => {
            assert!(
                reason.contains("no progress"),
                "the reason names the silence that expired, not a total cap: {reason}"
            );
            assert!(
                reason.contains("250"),
                "the reason names the cap that expired: {reason}"
            );
            assert!(
                reason.contains("partial transcript"),
                "the partial text is quoted: {reason}"
            );
        }
        other => panic!("expected Blocked, got {other:?}"),
    }

    let records = fake.records();
    let interrupt = records
        .iter()
        .rposition(|record| {
            record.cmd == "harness" && record.method.as_deref() == Some("interrupt")
        })
        .expect("the still-busy session was interrupted");
    let transcript = records
        .iter()
        .rposition(|record| {
            record.cmd == "harness" && record.method.as_deref() == Some("transcript")
        })
        .expect("the partial transcript was fetched");
    assert!(
        interrupt < transcript,
        "interrupt before the partial transcript: {records:#?}"
    );
    assert_eq!(
        records[interrupt].params()["session"],
        json!(SESSION),
        "the interrupt names the session"
    );
}

#[test]
fn a_connection_that_dies_is_an_error_not_a_panic() {
    let fake = FakeDoor::start(Mode::CloseAfterAuth);
    let mut harness =
        DoorHarness::connect(fake.port(), TOKEN).expect("the welcome is read before the hang-up");

    let outcome = harness.run(&step_request(PROMPT));

    assert!(
        outcome.is_err(),
        "a closed door is an error the caller can report: {outcome:?}"
    );
}
