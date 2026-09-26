//! Client for the Apple Foundation Models bridge (`native/AppleBridge.swift`).
//!
//! The bridge is a small Swift executable that wraps `LanguageModelSession`.
//! This crate spawns it in persistent serve mode: one JSON request per stdin
//! line, one response per stdout line, in order. Requests are serialized
//! through a single process because the on-device model is serial anyway —
//! callers beyond `max_pending` get [`Error::QueueFull`] instead of piling up.
//!
//! Product-neutral: the host picks the bridge binary, prompts, and schemas.

mod availability;
mod platform;

pub use availability::{
    Availability, Explanation, Reason, APPLE_INTELLIGENCE_SETTINGS_PATH,
    APPLE_INTELLIGENCE_SETTINGS_URL, SOFTWARE_UPDATE_SETTINGS_PATH, SOFTWARE_UPDATE_SETTINGS_URL,
};
use platform::os_guard;
pub use platform::platform_check;

use serde_json::Value;
use std::collections::HashMap;
use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
type BridgeInput = UnixStream;
#[cfg(not(unix))]
type BridgeInput = std::process::ChildStdin;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Swift source of the bridge, for hosts that want to compile it themselves.
pub const SWIFT_SOURCE: &str = include_str!("../native/AppleBridge.swift");

const MAX_PROMPT_BYTES: usize = 32_768;
const MAX_INSTRUCTIONS_BYTES: usize = 4_096;
const MAX_RESPONSE_LINE: usize = 4_194_304;
const MAX_REQUEST_LINE: usize = 1_048_576;
const MAX_OUTPUT_BYTES: usize = 262_144;
const CHECK_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug)]
#[non_exhaustive]
pub enum Error {
    /// Not macOS, so there is no bridge to run.
    Unsupported(String),
    /// The on-device model can't be used, for a typed reason: the Mac, macOS,
    /// Apple Intelligence settings, or model download state. See
    /// [`Reason::explain`] for the copy a person reads.
    Unavailable(Reason),
    Spawn(std::io::Error),
    Io(std::io::Error),
    /// The request exceeded its deadline; the bridge was killed and will
    /// be respawned by the next request.
    Timeout,
    /// `max_pending` callers are already admitted, including the active call.
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
            Self::Unavailable(reason) => {
                let summary = reason.explain().summary;
                write!(f, "{}", summary.trim_end_matches('.'))
            }
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

impl Error {
    /// The typed reason when the model or its helper can't be used.
    pub fn reason(&self) -> Option<Reason> {
        match self {
            Self::Unavailable(reason) => Some(*reason),
            _ => None,
        }
    }

    /// Copy for the person when the error has a known fix, else `None`.
    pub fn explain(&self) -> Option<Explanation> {
        self.reason().map(Reason::explain)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Options {
    /// Timeout per attempt: response waiting for legacy calls, or shared stdin
    /// write + response waiting for no-retry calls. Queue waiting and process
    /// spawn are outside this bound.
    pub request_timeout: Duration,
    /// Maximum admitted callers, including the in-flight request. Set to 1
    /// to reject concurrent callers with `QueueFull` rather than queue them.
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
    stdin: BridgeInput,
    pending: PendingMap,
}

fn spawn_conn(argv: &[String]) -> Result<Conn> {
    os_guard()?;
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    // A socket still supplies ordinary stdin bytes to the bridge, while safe
    // std write timeouts let strict calls bound delivery without writer threads.
    #[cfg(unix)]
    let (stdin, input) = UnixStream::pair().map_err(Error::Io)?;
    #[cfg(unix)]
    cmd.stdin(Stdio::from(std::os::fd::OwnedFd::from(input)));
    #[cfg(not(unix))]
    cmd.stdin(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn().map_err(Error::Spawn)?;
    #[cfg(not(unix))]
    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
    let reader_pending = Arc::clone(&pending);
    thread::spawn(move || {
        let mut reader = BufReader::new(stdout);
        loop {
            let mut line = Vec::new();
            match Read::by_ref(&mut reader)
                .take((MAX_RESPONSE_LINE + 1) as u64)
                .read_until(b'\n', &mut line)
            {
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
        os_guard()?;
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

    /// Execute a request with the legacy reconnect policy. A write failure or
    /// lost response may cause the request to be sent again; this is not an
    /// at-most-once operation. Use [`Self::request_no_retry`] when admission
    /// may already have happened before an error.
    ///
    /// Blocks while another request is in flight. Beyond `max_pending`
    /// admitted callers (including the active call), returns [`Error::QueueFull`].
    pub fn request(&self, request: &Request) -> Result<Value> {
        self.request_with_timeout(request, self.options.request_timeout)
    }

    /// Execute with the legacy reconnect policy and a response-wait timeout
    /// override. The timeout excludes queue waiting, spawn, and stdin writes,
    /// and each reconnect attempt receives the same timeout.
    pub fn request_with_timeout(&self, request: &Request, timeout: Duration) -> Result<Value> {
        self.request_impl(request, timeout, true)
    }

    /// Submit at most once, preserving a warm connection after a valid response.
    /// An already-dead idle connection may be replaced before submission. Once
    /// writing starts, an I/O error or lost response closes the connection and
    /// returns without reconnecting or resubmitting this request. Such an error
    /// does not prove the bridge failed to admit or complete the operation.
    ///
    /// A later explicit call is a new submission; this method does not deduplicate
    /// requests or reconcile uncertain outcomes. Queue and timeout semantics are
    /// the same as [`Self::request_no_retry_with_timeout`].
    pub fn request_no_retry(&self, request: &Request) -> Result<Value> {
        self.request_no_retry_with_timeout(request, self.options.request_timeout)
    }

    /// Submit at most once with one timeout covering stdin delivery and response
    /// waiting. Queue waiting and process spawn are outside this I/O deadline.
    /// Set [`Options::max_pending`] to 1 to reject concurrent callers instead of
    /// queuing them. A timeout kills and joins the owned bridge, but cannot prove
    /// whether generation happened before the response was lost.
    pub fn request_no_retry_with_timeout(
        &self,
        request: &Request,
        timeout: Duration,
    ) -> Result<Value> {
        self.request_impl(request, timeout, false)
    }

    fn request_impl(
        &self,
        request: &Request,
        timeout: Duration,
        retry_disconnect: bool,
    ) -> Result<Value> {
        request.validate()?;
        if timeout.is_zero() {
            return Err(Error::Timeout);
        }
        let waiters = self.waiters.fetch_add(1, Ordering::SeqCst);
        if waiters >= self.options.max_pending {
            self.waiters.fetch_sub(1, Ordering::SeqCst);
            return Err(Error::QueueFull);
        }
        let result = self.request_inner(request, timeout, retry_disconnect);
        self.waiters.fetch_sub(1, Ordering::SeqCst);
        result
    }

    fn request_inner(
        &self,
        request: &Request,
        timeout: Duration,
        retry_disconnect: bool,
    ) -> Result<Value> {
        let mut guard = self.conn.lock().unwrap();
        let mut attempts = 0u8;
        loop {
            if conn_dead(guard.as_mut()) {
                kill_conn(guard.take());
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
            let line = encode_request(id, request)?;
            let io_deadline = if retry_disconnect {
                None
            } else {
                Some(
                    Instant::now()
                        .checked_add(timeout)
                        .ok_or_else(|| Error::Protocol("request timeout out of range".into()))?,
                )
            };
            let (tx, rx) = mpsc::channel();
            conn.pending.lock().unwrap().insert(id, tx);
            let wrote = write_request(&mut conn.stdin, &line, io_deadline);
            if let Err(error) = wrote {
                conn.pending.lock().unwrap().remove(&id);
                kill_conn(guard.take());
                if !retry_disconnect {
                    return Err(error);
                }
                continue;
            }
            let remaining = io_deadline.map_or(timeout, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            match rx.recv_timeout(remaining) {
                Ok(resp) => return finish(id, resp),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    kill_conn(guard.take());
                    return Err(Error::Timeout);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    kill_conn(guard.take());
                    if !retry_disconnect {
                        return Err(Error::Protocol(
                            "bridge disconnected while awaiting response".into(),
                        ));
                    }
                    continue;
                }
            }
        }
    }
}

// Serialize through a bounded writer, not a to_vec followed by a size check.
// This is the bridge's existing request-line limit, excluding the newline.
fn encode_request(id: u64, request: &Request) -> Result<Vec<u8>> {
    // Borrow schema/input fields so a foreign oversized schema is not cloned
    // before the bounded serializer rejects it.
    #[derive(serde::Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Wire<'a> {
        id: u64,
        prompt: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        instructions: Option<&'a str>,
        #[serde(skip_serializing_if = "Option::is_none")]
        schema: Option<&'a Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        expect_json: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        max_output_bytes: Option<usize>,
    }
    let message = Wire {
        id,
        prompt: &request.prompt,
        instructions: request.instructions.as_deref(),
        schema: request.schema.as_ref(),
        expect_json: request.expect_json.then_some(true),
        max_output_bytes: request.max_output_bytes,
    };
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > MAX_REQUEST_LINE.saturating_sub(self.0.len()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "request exceeds 1048576 bytes",
                ));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Bounded(Vec::new());
    serde_json::to_writer(&mut output, &message)
        .map_err(|error| Error::Protocol(error.to_string()))?;
    output.0.push(b'\n');
    Ok(output.0)
}

fn write_request(input: &mut BridgeInput, bytes: &[u8], deadline: Option<Instant>) -> Result<()> {
    #[cfg(unix)]
    {
        let Some(deadline) = deadline else {
            input.set_write_timeout(None).map_err(Error::Io)?;
            return input
                .write_all(bytes)
                .and_then(|()| input.flush())
                .map_err(Error::Io);
        };
        let mut remaining_bytes = bytes;
        while !remaining_bytes.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }
            input
                .set_write_timeout(Some(remaining))
                .map_err(Error::Io)?;
            match input.write(remaining_bytes) {
                Ok(0) => {
                    return Err(Error::Io(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "bridge stdin write returned zero",
                    )))
                }
                Ok(count) => remaining_bytes = &remaining_bytes[count..],
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(Error::Timeout)
                }
                Err(error) => return Err(Error::Io(error)),
            }
        }
        // UnixStream is unbuffered: no additional flush can outlive the deadline.
        Ok(())
    }
    #[cfg(not(unix))]
    {
        // Constructors reject this platform before a connection is created.
        let _ = deadline;
        input
            .write_all(bytes)
            .and_then(|()| input.flush())
            .map_err(Error::Io)
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
        (Some(false), _, Some(err)) => {
            let code = err.get("code").and_then(Value::as_str).unwrap_or("unknown");
            if code == "modelUnavailable" {
                // Bridges before 0.2.0 send no reason; the code alone still
                // means "unavailable", not a generation failure.
                let reason = err.get("reason").and_then(Value::as_str);
                return Err(Error::Unavailable(
                    reason.map_or(Reason::Unavailable, Reason::from_wire),
                ));
            }
            Err(Error::Bridge(code.to_string()))
        }
        _ => Err(Error::Protocol(format!("malformed response for id {id}"))),
    }
}

fn run_capture(
    argv: &[String],
    extra_arg: &str,
    input: &[u8],
    timeout: Duration,
) -> Result<(bool, Vec<u8>, Vec<u8>)> {
    os_guard()?;
    if argv.is_empty() {
        return Err(Error::Protocol("invalid bridge argv".into()));
    }
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

/// Run `bridge --check` and return whether the model can be used and, if
/// not, why.
///
/// Every known "can't be used" state comes back as `Ok` with a typed
/// [`Reason`], never as an error: the bridge's own reasons, plus
/// [`Reason::HelperMissing`] when the executable doesn't exist and the
/// [`platform_check`] reason when the bridge can't start on this Mac (an Intel
/// Mac, or macOS before 26). `Err` means the check itself failed: a timeout,
/// an unexpected spawn error, or off-protocol output.
pub fn check(argv: &[String]) -> Result<Availability> {
    let (ok, out, err) = match run_capture(argv, "--check", b"", CHECK_TIMEOUT) {
        Ok(result) => result,
        Err(Error::Spawn(error)) => {
            if let Err(Error::Unavailable(reason)) = platform_check() {
                return Ok(Availability::unavailable(reason));
            }
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(Availability::unavailable(Reason::HelperMissing));
            }
            return Err(Error::Spawn(error));
        }
        Err(error) => return Err(error),
    };
    let envelope = if ok {
        serde_json::from_slice::<Value>(&out).ok()
    } else {
        None
    };
    let Some(v) = envelope else {
        // A bridge built for macOS 26 on Apple silicon dies in the loader on
        // an older or Intel Mac; say that rather than "protocol violation".
        if let Err(Error::Unavailable(reason)) = platform_check() {
            return Ok(Availability::unavailable(reason));
        }
        if ok {
            return Err(Error::Protocol("check printed invalid JSON".into()));
        }
        let v: Value = serde_json::from_slice(&err).unwrap_or_default();
        return Err(Error::Protocol(format!(
            "check failed: {}",
            v.get("reason")
                .or_else(|| v.get("error"))
                .unwrap_or(&Value::Null)
        )));
    };
    if v.get("available").and_then(Value::as_bool) == Some(true) {
        return Ok(Availability::ready());
    }
    let reason = v
        .get("reason")
        .and_then(Value::as_str)
        .map_or(Reason::Unavailable, Reason::from_wire);
    Ok(Availability::unavailable(reason))
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
    os_guard()?;
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
