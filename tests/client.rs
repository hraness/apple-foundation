// The production client intentionally rejects non-macOS platforms before I/O.
#![cfg(target_os = "macos")]

use apple_foundation::{
    check, platform_check, schema_check, Availability, Bridge, Error, Options, Reason, Request,
};
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

fn fake_bridge() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let target = std::env::var("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join("target"));
    let path = target.join("debug/examples/fake_bridge");
    assert!(path.exists(), "fake_bridge example not built: {path:?}");
    path
}

fn argv(extra: &[&str]) -> Vec<String> {
    let mut v = vec![fake_bridge().to_string_lossy().into_owned()];
    v.extend(extra.iter().map(|s| s.to_string()));
    v
}

fn options(timeout_ms: u64, max_pending: usize) -> Options {
    Options {
        request_timeout: Duration::from_millis(timeout_ms),
        max_pending,
    }
}

#[test]
fn check_reports_availability() {
    let a = check(&argv(&[])).unwrap();
    assert!(a.available);
}

#[test]
fn check_carries_each_bridge_reason() {
    for reason in [
        "deviceNotEligible",
        "appleIntelligenceNotEnabled",
        "modelNotReady",
        "requiresMacOS26",
        "unavailable",
    ] {
        let a = check(&argv(&["--check-reason", reason])).unwrap();
        assert_eq!(a, Availability::unavailable(Reason::from_wire(reason)));
        assert_eq!(a.reason.unwrap().as_str(), reason);
        assert!(a.explain().is_some());
    }
}

#[test]
fn check_without_or_with_unknown_reason_is_unavailable() {
    let a = check(&argv(&["--check-reason", "-"])).unwrap();
    assert_eq!(a, Availability::unavailable(Reason::Unavailable));
    let a = check(&argv(&["--check-reason", "brandNewReason"])).unwrap();
    assert_eq!(a, Availability::unavailable(Reason::Unavailable));
}

/// What `check` should say when the bridge can't run at all: the platform
/// reason on an old or Intel Mac (CI runs macOS 14), otherwise `fallback`.
fn platform_or(fallback: Reason) -> Availability {
    match platform_check() {
        Err(Error::Unavailable(reason)) => Availability::unavailable(reason),
        _ => Availability::unavailable(fallback),
    }
}

#[test]
fn check_reports_missing_helper_instead_of_a_spawn_error() {
    let missing = vec!["/nonexistent/apple-foundation/apple-bridge".to_string()];
    let a = check(&missing).unwrap();
    assert_eq!(a, platform_or(Reason::HelperMissing));
}

#[test]
fn check_explains_a_bridge_that_cannot_start() {
    match (check(&argv(&["--check-crash"])), platform_check()) {
        (Ok(a), Err(Error::Unavailable(reason))) => {
            assert_eq!(a, Availability::unavailable(reason))
        }
        (Err(Error::Protocol(message)), Ok(())) => assert!(message.starts_with("check failed")),
        other => panic!("unexpected: {other:?}"),
    }
    match (check(&argv(&["--check-garbage"])), platform_check()) {
        (Ok(a), Err(Error::Unavailable(reason))) => {
            assert_eq!(a, Availability::unavailable(reason))
        }
        (Err(Error::Protocol(message)), Ok(())) => assert!(message.contains("invalid JSON")),
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn check_rejects_empty_argv_without_panicking() {
    assert!(matches!(check(&[]), Err(Error::Protocol(_))));
}

#[test]
fn request_time_unavailability_carries_the_reason() {
    let bridge = Bridge::new(&argv(&["--unavailable", "modelNotReady"])).unwrap();
    match bridge.request(&Request::text("hello")) {
        Err(error @ Error::Unavailable(Reason::ModelNotReady)) => {
            assert_eq!(error.reason(), Some(Reason::ModelNotReady));
            assert!(error.explain().unwrap().temporary);
            assert_eq!(
                error.to_string(),
                "Apple's on-device model is still downloading"
            );
        }
        other => panic!("expected typed unavailability, got {other:?}"),
    }
    let bridge = Bridge::new(&argv(&["--unavailable-bare"])).unwrap();
    assert!(matches!(
        bridge.request_no_retry(&Request::text("hello")),
        Err(Error::Unavailable(Reason::Unavailable))
    ));
}

#[test]
fn schema_check_accepts_and_rejects() {
    schema_check(&argv(&[]), &json!({"type": "object", "properties": {}})).unwrap();
    match schema_check(&argv(&[]), &json!({"type": "bogus"})) {
        Err(Error::Bridge(code)) => assert_eq!(code, "unsupportedSchemaType"),
        other => panic!("expected bridge rejection, got {other:?}"),
    }
}

#[test]
fn request_round_trip() {
    let bridge = Bridge::new(&argv(&[])).unwrap();
    let value = bridge.request(&Request::text("hello")).unwrap();
    assert_eq!(value, json!({"echo": "hello"}));
}

#[test]
fn guided_request_carries_schema() {
    let bridge = Bridge::new(&argv(&[])).unwrap();
    let value = bridge
        .request(&Request::guided("pick", json!({"enum": ["a", "b"]})))
        .unwrap();
    assert_eq!(value, json!({"echo": "pick"}));
}

#[test]
fn error_envelope_maps_to_bridge_error() {
    let bridge = Bridge::new(&argv(&[])).unwrap();
    match bridge.request(&Request::text("fail")) {
        Err(Error::Bridge(code)) => assert_eq!(code, "generationFailed"),
        other => panic!("expected bridge error, got {other:?}"),
    }
}

#[test]
fn requests_serialize_across_threads() {
    let bridge = std::sync::Arc::new(Bridge::new(&argv(&[])).unwrap());
    let mut handles = Vec::new();
    for i in 0..8 {
        let bridge = std::sync::Arc::clone(&bridge);
        handles.push(std::thread::spawn(move || {
            bridge
                .request(&Request::text(format!("prompt-{i}")))
                .unwrap()
        }));
    }
    for (i, h) in handles.into_iter().enumerate() {
        assert_eq!(h.join().unwrap(), json!({"echo": format!("prompt-{i}")}));
    }
}

#[test]
fn queue_full_when_pending_exceeds_cap() {
    let bridge = std::sync::Arc::new(
        Bridge::with_options(&argv(&["--hang-on", "1"]), options(1_500, 1)).unwrap(),
    );
    let blocked = {
        let bridge = std::sync::Arc::clone(&bridge);
        std::thread::spawn(move || bridge.request(&Request::text("slow")))
    };
    std::thread::sleep(Duration::from_millis(200));
    match bridge.request(&Request::text("fast")) {
        Err(Error::QueueFull) => {}
        other => panic!("expected queue full, got {other:?}"),
    }
    let _ = blocked.join();
}

#[test]
fn timeout_kills_bridge_and_next_request_respawns() {
    let bridge = Bridge::with_options(&argv(&["--hang-on", "1"]), options(400, 8)).unwrap();
    match bridge.request(&Request::text("slow")) {
        Err(Error::Timeout) => {}
        other => panic!("expected timeout, got {other:?}"),
    }
    let value = bridge.request(&Request::text("after")).unwrap();
    assert_eq!(value, json!({"echo": "after"}));
}

#[test]
fn dead_bridge_respawns_on_next_request() {
    let bridge = Bridge::new(&argv(&["--die-after", "1"])).unwrap();
    bridge.request(&Request::text("first")).unwrap();
    // The fake exits after one request; the next call must respawn it.
    let value = bridge.request(&Request::text("second")).unwrap();
    assert_eq!(value, json!({"echo": "second"}));
}

#[test]
fn unmatched_response_id_times_out() {
    let bridge = Bridge::with_options(&argv(&["--bad-id", "1"]), options(400, 8)).unwrap();
    match bridge.request(&Request::text("lost")) {
        Err(Error::Timeout) => {}
        other => panic!("expected timeout on dropped response, got {other:?}"),
    }
}

#[test]
fn invalid_argv_and_request_bounds() {
    assert!(Bridge::new(&[]).is_err());
    let bridge = Bridge::new(&argv(&[])).unwrap();
    assert!(bridge.request(&Request::text("")).is_err());
    assert!(bridge.request(&Request::text("x".repeat(33_000))).is_err());
    assert!(bridge
        .request(&Request {
            max_output_bytes: Some(0),
            ..Request::text("hi")
        })
        .is_err());
}

#[test]
fn swift_source_is_embedded() {
    assert!(apple_foundation::SWIFT_SOURCE.contains("FoundationModels"));
}

struct FixtureDirectory(PathBuf);

impl FixtureDirectory {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "apple-foundation-strict-{}-{id}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn events(&self) -> Vec<String> {
        std::fs::read_to_string(self.0.join("events"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
    fn argv(&self, extra: &[&str]) -> Vec<String> {
        let mut arguments = argv(&["--audit"]);
        arguments.push(self.0.join("events").to_string_lossy().into_owned());
        arguments.extend(extra.iter().map(|value| (*value).to_owned()));
        arguments
    }
}

impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn no_retry_preserves_response_loss_after_one_admission() {
    let fixture = FixtureDirectory::new();
    let bridge = Bridge::new(&fixture.argv(&["--drop-response"])).unwrap();
    let error = bridge.request_no_retry(&Request::text("admit then exit"));
    assert!(
        matches!(error, Err(Error::Protocol(ref message)) if message == "bridge disconnected while awaiting response")
    );
    assert_eq!(fixture.events(), ["spawn", "admit"]);
}

#[test]
fn no_retry_preserves_partial_write_error_without_reconnect() {
    let fixture = FixtureDirectory::new();
    let bridge = Bridge::new(&fixture.argv(&["--exit-after-prefix"])).unwrap();
    // Larger than the macOS socket buffer: the fixture takes one byte, records
    // that possible admission boundary, then exits while the client writes.
    let request = Request::guided("partial write", json!({"description":"x".repeat(262_144)}));
    let error = bridge.request_no_retry(&request);
    assert!(matches!(error, Err(Error::Io(_))), "{error:?}");
    assert_eq!(fixture.events(), ["spawn", "prefix"]);
}

#[test]
fn no_retry_does_not_mask_response_loss_with_a_later_spawn_failure() {
    let fixture = FixtureDirectory::new();
    let executable = fixture.0.join("owned-bridge");
    std::fs::copy(fake_bridge(), &executable).unwrap();
    let mut arguments = fixture.argv(&["--unlink-self-after-admit"]);
    arguments[0] = executable.to_string_lossy().into_owned();
    let bridge = Bridge::new(&arguments).unwrap();
    let error = bridge.request_no_retry(&Request::text("admit then retire executable"));
    assert!(matches!(error, Err(Error::Protocol(_))), "{error:?}");
    assert!(!executable.exists());
    assert_eq!(fixture.events(), ["spawn", "admit"]);
}

#[test]
fn no_retry_reuses_warm_connection_and_preserves_bridge_rejection() {
    let fixture = FixtureDirectory::new();
    let bridge = Bridge::new(&fixture.argv(&[])).unwrap();
    assert_eq!(
        bridge.request_no_retry(&Request::text("first")).unwrap(),
        json!({"echo":"first"})
    );
    assert!(
        matches!(bridge.request_no_retry(&Request::text("fail")), Err(Error::Bridge(code)) if code == "generationFailed")
    );
    assert_eq!(
        bridge
            .request_no_retry_with_timeout(&Request::text("last"), Duration::from_secs(1))
            .unwrap(),
        json!({"echo":"last"})
    );
    assert_eq!(fixture.events(), ["spawn", "admit", "admit", "admit"]);
}

#[test]
fn no_retry_rejects_invalid_requests_before_spawning() {
    let fixture = FixtureDirectory::new();
    let bridge = Bridge::new(&fixture.argv(&[])).unwrap();
    assert!(matches!(
        bridge.request_no_retry(&Request::text("")),
        Err(Error::Protocol(_))
    ));
    assert!(matches!(
        bridge.request_no_retry_with_timeout(&Request::text("valid"), Duration::ZERO),
        Err(Error::Timeout)
    ));
    assert!(fixture.events().is_empty());
}

#[test]
fn legacy_requests_keep_the_existing_reconnect_policy() {
    let fixture = FixtureDirectory::new();
    let bridge = Bridge::new(&fixture.argv(&["--drop-response"])).unwrap();
    assert!(matches!(
        bridge.request(&Request::text("legacy")),
        Err(Error::Protocol(_))
    ));
    assert_eq!(fixture.events(), ["spawn", "admit", "spawn", "admit"]);
}

#[test]
fn no_retry_bounds_stalled_stdin_and_never_resubmits() {
    let fixture = FixtureDirectory::new();
    let bridge = Bridge::new(&fixture.argv(&["--stall-after-prefix"])).unwrap();
    let request = Request::guided("stalled write", json!({"description":"x".repeat(900_000)}));
    let start = std::time::Instant::now();
    assert!(matches!(
        bridge.request_no_retry_with_timeout(&request, Duration::from_millis(200)),
        Err(Error::Timeout)
    ));
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(fixture.events(), ["spawn", "prefix"]);
}

#[test]
fn oversized_response_without_newline_is_bounded_and_not_retried() {
    let fixture = FixtureDirectory::new();
    let bridge = Bridge::new(&fixture.argv(&["--oversize-response"])).unwrap();
    let start = std::time::Instant::now();
    let result = bridge.request_no_retry_with_timeout(
        &Request::text("oversized response"),
        Duration::from_secs(5),
    );
    assert!(matches!(result, Err(Error::Protocol(_))), "{result:?}");
    assert!(start.elapsed() < Duration::from_secs(3));
    assert_eq!(fixture.events(), ["spawn", "admit"]);
}

#[test]
fn oversized_schema_is_rejected_before_any_request_bytes() {
    let fixture = FixtureDirectory::new();
    let bridge = Bridge::new(&fixture.argv(&[])).unwrap();
    let request = Request::guided("too large", json!({"description":"x".repeat(1_048_576)}));
    assert!(
        matches!(bridge.request_no_retry(&request), Err(Error::Protocol(message)) if message.contains("request exceeds 1048576 bytes"))
    );
    // The lazy process may have been spawned, but no request was admitted.
    drop(bridge);
    assert!(!fixture.events().iter().any(|event| event == "admit"));
}

/// Runs only when `APPLE_FOUNDATION_LIVE_BRIDGE` names a real bridge built
/// with `sh scripts/build-bridge.sh`: the answer must be typed either way.
#[test]
fn live_bridge_check_is_typed() {
    let Some(path) = std::env::var_os("APPLE_FOUNDATION_LIVE_BRIDGE") else {
        return;
    };
    let a = check(&[path.to_string_lossy().into_owned()]).unwrap();
    assert_eq!(a.available, a.reason.is_none());
    eprintln!("live bridge: {a:?}");
}

const HELP_GOLDEN: &str = include_str!("golden/bridge-help.txt");

#[test]
fn swift_help_text_matches_golden() {
    // CI can't build the bridge (no macOS 26 SDK), so pin the source too.
    for line in HELP_GOLDEN.lines().filter(|l| !l.is_empty()) {
        let swift = line.replace("{name}", "\\(name)");
        assert!(
            apple_foundation::SWIFT_SOURCE.contains(&swift),
            "help line missing from AppleBridge.swift: {line}"
        );
        assert!(line.len() <= 80, "help line over 80 columns: {line}");
    }
}

#[test]
fn swift_bridge_version_moves_with_the_crate() {
    let expected = format!("let bridgeVersion = \"{}\"", env!("CARGO_PKG_VERSION"));
    assert!(apple_foundation::SWIFT_SOURCE.contains(&expected));
}

fn live_bridge() -> Option<PathBuf> {
    std::env::var_os("APPLE_FOUNDATION_LIVE_BRIDGE").map(PathBuf::from)
}

fn run_live(bridge: &std::path::Path, args: &[&str], env: &[(&str, &str)]) -> std::process::Output {
    let mut cmd = std::process::Command::new(bridge);
    cmd.args(args).stdin(std::process::Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().unwrap()
}

/// Live CLI checks for a bridge built with `sh scripts/build-bridge.sh`.
#[test]
fn live_bridge_help_version_and_errors() {
    let Some(bridge) = live_bridge() else { return };
    let name = bridge.file_name().unwrap().to_string_lossy().into_owned();
    for flag in ["--help", "-h", "help"] {
        for env in [&[][..], &[("NO_COLOR", "1"), ("TERM", "dumb")][..]] {
            let out = run_live(&bridge, &[flag], env);
            assert!(out.status.success(), "{flag} exit {:?}", out.status);
            assert_eq!(
                String::from_utf8(out.stdout).unwrap(),
                HELP_GOLDEN.replace("{name}", &name)
            );
            assert!(out.stderr.is_empty());
        }
    }
    for flag in ["--version", "-V"] {
        let out = run_live(&bridge, &[flag], &[]);
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            format!("{name} {}\n", env!("CARGO_PKG_VERSION"))
        );
    }
    // Not a terminal: programs keep the JSON error and exit 1.
    let out = run_live(&bridge, &["--bogus"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert_eq!(
        String::from_utf8(out.stderr).unwrap(),
        "{\"error\":{\"code\":\"unknownArguments\"},\"ok\":false}\n"
    );
}

/// `apple-bridge --help | head -1`: a closed pipe ends it quietly.
#[test]
fn live_bridge_help_survives_a_closed_pipe() {
    let Some(bridge) = live_bridge() else { return };
    let mut child = std::process::Command::new(&bridge)
        .arg("--help")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    let out = child.wait_with_output().unwrap();
    assert!(
        out.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Runs only with `APPLE_FOUNDATION_LIVE_BUILD=1`: really compiles the bridge
/// into a temp directory (about ten seconds, macOS 26 + Xcode 26 only).
#[test]
fn live_ensure_bridge_builds_quietly_and_checks() {
    if std::env::var_os("APPLE_FOUNDATION_LIVE_BUILD").is_none() {
        return;
    }
    let fixture = FixtureDirectory::new();
    let install = fixture.0.join("bin/apple-bridge");
    assert!(!apple_foundation::bridge_is_current(&install));
    apple_foundation::build_tools_check().unwrap();
    let built = apple_foundation::ensure_bridge(&install).unwrap();
    assert!(apple_foundation::bridge_is_current(&built));
    let a = check(&[built.to_string_lossy().into_owned()]).unwrap();
    eprintln!("live build: {a:?}");
}
