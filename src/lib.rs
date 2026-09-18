//! Client for the Apple Foundation Models bridge (`native/AppleBridge.swift`).
//!
//! The bridge is a small Swift executable that wraps `LanguageModelSession`.
//! This crate spawns it in persistent serve mode: one JSON request per stdin
//! line, one response per stdout line, in order. Requests are serialized
//! through a single process because the on-device model is serial anyway —
//! callers beyond `max_pending` get [`Error::QueueFull`] instead of piling up.
//!
//! Product-neutral: the host picks the bridge binary, prompts, and schemas.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Swift source of the bridge, for hosts that want to compile it themselves.
pub const SWIFT_SOURCE: &str = include_str!("../native/AppleBridge.swift");

const MAX_PROMPT_BYTES: usize = 32_768;
const MAX_INSTRUCTIONS_BYTES: usize = 4_096;
const MAX_RESPONSE_LINE: usize = 4_194_304;
const MAX_OUTPUT_BYTES: usize = 262_144;
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
pub enum Error {
    /// Platform cannot run the bridge (not macOS, or bridge reports it).
    Unsupported(String),
    /// `--check` reports the on-device model is not usable right now.
    Unavailable(String),
    Spawn(std::io::Error),
    Io(std::io::Error),
    /// The request exceeded its deadline; the bridge was killed and will
    /// be respawned by the next request.
    Timeout,
    /// More than `max_pending` callers are already queued on this bridge.
    QueueFull,
    /// The bridge returned an error envelope; payload is its `error.code`.
    Bridge(String),
    /// The bridge emitted something off-protocol.
    Protocol(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(m) => write!(f, "apple bridge unsupported: {m}"),
            Self::Unavailable(m) => write!(f, "apple model unavailable: {m}"),
            Self::Spawn(e) => write!(f, "spawn apple bridge: {e}"),
            Self::Io(e) => write!(f, "apple bridge io: {e}"),
            Self::Timeout => write!(f, "apple bridge request timed out"),
            Self::QueueFull => write!(f, "apple bridge request queue is full"),
            Self::Bridge(code) => write!(f, "apple bridge error: {code}"),
            Self::Protocol(m) => write!(f, "apple bridge protocol violation: {m}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Options {
    /// Per-request deadline. The on-device model can take seconds to warm up
    /// on the first request in a fresh process.
    pub request_timeout: Duration,
    /// Maximum callers queued behind the one in-flight request.
    pub max_pending: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(120),
            max_pending: 64,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Availability {
    pub available: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub prompt: String,
    pub instructions: Option<String>,
    /// JSON-schema subset for guided generation (see spec/protocol.md).
    /// A schema the bridge cannot translate fails the request rather than
    /// silently downgrading to free text.
    pub schema: Option<Value>,
    /// With no schema: parse the free-text response as a JSON value.
    pub expect_json: bool,
    /// Response byte cap, 1..=262144. Defaults to 16384 bridge-side.
    pub max_output_bytes: Option<usize>,
}

impl Request {
    pub fn text(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            ..Self::default()
        }
    }
    pub fn guided(prompt: impl Into<String>, schema: Value) -> Self {
        Self {
            prompt: prompt.into(),
            schema: Some(schema),
            ..Self::default()
        }
    }
    fn validate(&self) -> Result<()> {
        if self.prompt.is_empty() || self.prompt.len() > MAX_PROMPT_BYTES {
            return Err(Error::Protocol("prompt empty or over 32768 bytes".into()));
        }
        if self
            .instructions
            .as_ref()
            .is_some_and(|s| s.len() > MAX_INSTRUCTIONS_BYTES)
        {
            return Err(Error::Protocol("instructions over 4096 bytes".into()));
        }
        if self
            .max_output_bytes
            .is_some_and(|n| n == 0 || n > MAX_OUTPUT_BYTES)
        {
            return Err(Error::Protocol("max_output_bytes out of range".into()));
        }
        Ok(())
    }
}

type PendingMap = Arc<Mutex<HashMap<u64, mpsc::Sender<Value>>>>;

struct Conn {
    child: Child,
    stdin: ChildStdin,
    pending: PendingMap,
}

fn platform_check() -> Result<()> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err(Error::Unsupported(
            "Apple Foundation Models requires macOS".into(),
        ))
    }
}

fn spawn_conn(argv: &[String]) -> Result<Conn> {
    platform_check()?;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(Error::Spawn)?;
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let reader_pending = Arc::clone(&pending);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = Vec::new();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if line.len() > MAX_RESPONSE_LINE {
                break;
            }
            let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            let id = map.get("id").and_then(Value::as_u64);
            let Some(id) = id else { continue };
            if let Some(tx) = reader_pending.lock().unwrap().remove(&id) {
                let _ = tx.send(Value::Object(map));
            }
        }
        // Reader exiting means the child is gone or off-protocol: drop every
        // pending sender so waiters fail instead of hanging.
        reader_pending.lock().unwrap().clear();
    });
    Ok(Conn {
        child,
        stdin,
        pending,
    })
}

/// One persistent bridge process. `request` calls are serialized; the first
/// call spawns the process and later calls respawn it if it died.
pub struct Bridge {
    argv: Vec<String>,
    options: Options,
    conn: Mutex<Option<Conn>>,
    waiters: AtomicUsize,
    next_id: AtomicU64,
}

impl Bridge {
    /// `argv` is the bridge executable plus any fixed arguments (bounded).
    /// The process is spawned lazily on the first request.
    pub fn new(argv: &[String]) -> Result<Self> {
        Self::with_options(argv, Options::default())
    }

    pub fn with_options(argv: &[String], options: Options) -> Result<Self> {
        platform_check()?;
        if argv.is_empty()
            || argv.len() > 8
            || argv
                .iter()
                .any(|a| a.is_empty() || a.len() > 4096 || a.contains('\0'))
        {
            return Err(Error::Protocol("invalid bridge argv".into()));
        }
        if options.request_timeout.is_zero()
            || options.request_timeout > Duration::from_secs(600)
            || options.max_pending == 0
            || options.max_pending > 1024
        {
            return Err(Error::Protocol("invalid bridge options".into()));
        }
        Ok(Self {
            argv: argv.to_vec(),
            options,
            conn: Mutex::new(None),
            waiters: AtomicUsize::new(0),
            next_id: AtomicU64::new(1),
        })
    }

    /// Execute one request. Blocks while another request is in flight —
    /// that serialization is the queue. Beyond `max_pending` queued
    /// callers, returns [`Error::QueueFull`].
    pub fn request(&self, request: &Request) -> Result<Value> {
        self.request_with_timeout(request, self.options.request_timeout)
    }

    /// Execute one request with a per-call deadline override (for hosts
    /// whose effect budget varies per request).
    pub fn request_with_timeout(&self, request: &Request, timeout: Duration) -> Result<Value> {
        request.validate()?;
        if timeout.is_zero() {
            return Err(Error::Timeout);
        }
        let waiters = self.waiters.fetch_add(1, Ordering::SeqCst);
        if waiters >= self.options.max_pending {
            self.waiters.fetch_sub(1, Ordering::SeqCst);
            return Err(Error::QueueFull);
        }
        let result = self.request_inner(request, timeout);
        self.waiters.fetch_sub(1, Ordering::SeqCst);
        result
    }

    fn request_inner(&self, request: &Request, timeout: Duration) -> Result<Value> {
        let mut guard = self.conn.lock().unwrap();
        let mut attempts = 0u8;
        loop {
            if conn_dead(guard.as_mut()) {
                *guard = None;
            }
            if guard.is_none() {
                if attempts >= 2 {
                    return Err(Error::Protocol("bridge died during request".into()));
                }
                *guard = Some(spawn_conn(&self.argv)?);
            }
            attempts += 1;
            let conn = guard.as_mut().unwrap();
            let id = self.next_id.fetch_add(1, Ordering::SeqCst);
            let mut msg = json!({"id": id, "prompt": request.prompt});
            if let Some(instructions) = &request.instructions {
                msg["instructions"] = json!(instructions);
            }
            if let Some(schema) = &request.schema {
                msg["schema"] = schema.clone();
            }
            if request.expect_json {
                msg["expectJson"] = json!(true);
            }
            if let Some(max) = request.max_output_bytes {
                msg["maxOutputBytes"] = json!(max);
            }
            let mut line = serde_json::to_vec(&msg).map_err(|e| Error::Protocol(e.to_string()))?;
            line.push(b'\n');
            let (tx, rx) = mpsc::channel();
            conn.pending.lock().unwrap().insert(id, tx);
            let wrote = conn
                .stdin
                .write_all(&line)
                .and_then(|()| conn.stdin.flush());
            if wrote.is_err() {
                conn.pending.lock().unwrap().remove(&id);
                kill_conn(guard.take());
                continue;
            }
            match rx.recv_timeout(timeout) {
                Ok(resp) => return finish(id, resp),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    kill_conn(guard.take());
                    return Err(Error::Timeout);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    kill_conn(guard.take());
                    continue;
                }
            }
        }
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.get_mut().unwrap().take() {
            kill_conn(Some(conn));
        }
    }
}

fn conn_dead(conn: Option<&mut Conn>) -> bool {
    match conn {
        None => false,
        Some(c) => matches!(c.child.try_wait(), Ok(Some(_)) | Err(_)),
    }
}

fn kill_conn(conn: Option<Conn>) {
    if let Some(mut conn) = conn {
        let _ = conn.child.kill();
        let _ = conn.child.wait();
        conn.pending.lock().unwrap().clear();
    }
}

fn finish(id: u64, resp: Value) -> Result<Value> {
    match (
        resp.get("ok").and_then(Value::as_bool),
        resp.get("value"),
        resp.get("error"),
    ) {
        (Some(true), Some(value), _) => Ok(value.clone()),
        (Some(false), _, Some(err)) => Err(Error::Bridge(
            err.get("code")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string(),
        )),
        _ => Err(Error::Protocol(format!("malformed response for id {id}"))),
    }
}

fn run_capture(
    argv: &[String],
    extra_arg: &str,
    input: &[u8],
    timeout: Duration,
) -> Result<(bool, Vec<u8>, Vec<u8>)> {
    platform_check()?;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .arg(extra_arg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(Error::Spawn)?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input)
        .map_err(Error::Io)?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().map_err(Error::Io)? {
            Some(status) => {
                let mut out = Vec::new();
                let mut err = Vec::new();
                child
                    .stdout
                    .take()
                    .unwrap()
                    .read_to_end(&mut out)
                    .map_err(Error::Io)?;
                child
                    .stderr
                    .take()
                    .unwrap()
                    .read_to_end(&mut err)
                    .map_err(Error::Io)?;
                return Ok((status.success(), out, err));
            }
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Timeout);
            }
            None => thread::sleep(Duration::from_millis(25)),
        }
    }
}

/// Run `bridge --check` and parse the availability envelope.
pub fn check(argv: &[String]) -> Result<Availability> {
    let (ok, out, err) = run_capture(argv, "--check", b"", CHECK_TIMEOUT)?;
    if !ok {
        let v: Value = serde_json::from_slice(&err).unwrap_or_default();
        return Err(Error::Protocol(format!(
            "check failed: {}",
            v.get("reason")
                .or_else(|| v.get("error"))
                .unwrap_or(&Value::Null)
        )));
    }
    let v: Value = serde_json::from_slice(&out).map_err(|e| Error::Protocol(e.to_string()))?;
    if v.get("reason").and_then(Value::as_str) == Some("requiresMacOS26") {
        return Err(Error::Unsupported("requires macOS 26".into()));
    }
    Ok(Availability {
        available: v.get("available").and_then(Value::as_bool).unwrap_or(false),
        reason: v.get("reason").and_then(Value::as_str).map(str::to_string),
    })
}

/// Run `bridge --schema-check` against a JSON schema. `Err(Bridge(code))`
/// carries the bridge's rejection code (e.g. `schemaDepthExceeded`).
pub fn schema_check(argv: &[String], schema: &Value) -> Result<()> {
    let input = serde_json::to_vec(schema).map_err(|e| Error::Protocol(e.to_string()))?;
    let (ok, _, err) = run_capture(argv, "--schema-check", &input, CHECK_TIMEOUT)?;
    if ok {
        return Ok(());
    }
    let v: Value = serde_json::from_slice(&err).unwrap_or_default();
    let code = v
        .get("error")
        .and_then(|e| e.get("code"))
        .and_then(Value::as_str)
        .unwrap_or("invalidSchema");
    Err(Error::Bridge(code.to_string()))
}

/// Stamp written next to a built bridge recording which source it came from.
/// A version bump or source edit invalidates older installs.
fn source_stamp() -> String {
    format!("{}:{}", env!("CARGO_PKG_VERSION"), SWIFT_SOURCE.len())
}

/// Compile the bridge to `install` if missing or built from different source,
/// returning its path. Requires macOS with Xcode 26+ (`xcrun swiftc`). Builds
/// write to a unique temp file and atomically rename, so concurrent
/// first-builds race harmlessly to an identical binary.
pub fn ensure_bridge(install: &Path) -> Result<PathBuf> {
    platform_check()?;
    let stamp_path = install.with_extension("stamp");
    let fresh = install.is_file()
        && std::fs::read_to_string(&stamp_path).is_ok_and(|s| s == source_stamp());
    if fresh {
        return Ok(install.to_path_buf());
    }
    let dir = install
        .parent()
        .ok_or_else(|| Error::Protocol("install path has no parent".into()))?;
    std::fs::create_dir_all(dir).map_err(Error::Io)?;
    let src = std::env::temp_dir().join(format!("apple-bridge-{}.swift", std::process::id()));
    std::fs::write(&src, SWIFT_SOURCE).map_err(Error::Io)?;
    let tmp_out = dir.join(format!(".apple-bridge-{}.tmp", std::process::id()));
    let status = Command::new("xcrun")
        .args([
            "swiftc",
            "-parse-as-library",
            "-O",
            "-target",
            "arm64-apple-macosx26.0",
        ])
        .arg(&src)
        .arg("-o")
        .arg(&tmp_out)
        .status()
        .map_err(Error::Spawn)?;
    let _ = std::fs::remove_file(&src);
    if !status.success() {
        let _ = std::fs::remove_file(&tmp_out);
        return Err(Error::Unsupported("swiftc failed to build bridge".into()));
    }
    std::fs::rename(&tmp_out, install).map_err(Error::Io)?;
    let _ = std::fs::write(&stamp_path, source_stamp());
    Ok(install.to_path_buf())
}
