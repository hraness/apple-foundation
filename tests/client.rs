use apple_foundation::{check, schema_check, Bridge, Error, Options, Request};
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
