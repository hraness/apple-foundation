# apple-foundation

Use Apple's on-device Foundation Models from Rust, or from any language that can
start a process and exchange JSON lines with it. The model runs locally on Apple
Silicon Macs with macOS 26 or later, costs nothing per call, and needs no API
key. The bridge makes no network calls and never falls back to a cloud
provider.

The project has two parts:

- `native/AppleBridge.swift` is a small executable around
  `LanguageModelSession`. It reads one JSON request per line and writes one
  JSON response per line, in order, so one long-running process handles one
  generation at a time. Guided output translates a subset of JSON Schema into
  `DynamicGenerationSchema`, so the schema constrains the model while it
  generates. Modes: `--check` (availability), `--schema-check`, `--once`, and
  a persistent serve mode.
- `src/lib.rs` is the `apple-foundation` Rust client. It starts the bridge on
  first use, runs one request at a time with a size-limited queue (a full queue
  returns `QueueFull`), applies a per-request timeout, restarts the bridge
  after a failure, and can compile the embedded Swift source with
  `ensure_bridge()` when no binary is installed.

Wire contract: [`spec/protocol.md`](spec/protocol.md).

## Build

```sh
sh scripts/build-bridge.sh            # builds target/debug/apple-bridge
target/debug/apple-bridge --check     # {"available":true,...} on macOS 26+
cargo test                            # protocol + client tests (fake bridge)
```

A successful build does not mean the model is available. Run `--check` to see
live availability; it can report `modelNotReady`, `appleIntelligenceNotEnabled`,
or `deviceNotEligible` on real machines.

## Consumers

Rust hosts depend on the crate by immutable tag:

```toml
apple-foundation = { git = "https://github.com/hraness/apple-foundation", tag = "v0.1.3" }
```

and resolve the bridge binary their own way (installed path, env var, or
`ensure_bridge`):

```rust
use apple_foundation::{Bridge, Request};

let bridge = Bridge::new(&["/usr/local/bin/apple-bridge".to_string()])?;
let reply = bridge.request(&Request::text("Summarize this note in one sentence."))?;
```

Other languages can drive the executable's line protocol directly.

## When the model can't be used

`check` returns `Availability { available, reason }` with a typed `Reason`:
`AppleIntelligenceNotEnabled`, `ModelNotReady`, `DeviceNotEligible`,
`RequiresMacOS26`, `HelperMissing` (no bridge at that path), or
`Unavailable`. These are answers, not errors. A request made while the model
is unavailable fails with `Error::Unavailable(reason)`.

`reason.explain()` returns the words to show and the System Settings pane that
fixes it, so every host says the same thing:

```rust
use apple_foundation::check;

let availability = check(&["/usr/local/bin/apple-bridge".to_string()])?;
if let Some(help) = availability.explain() {
    eprintln!("⚠ {}", help.summary);   // Apple Intelligence is off.
    eprintln!("  {}", help.fix);       // Turn it on in System Settings › Apple Intelligence & Siri, then try again.
    if let Some(url) = help.settings_url {
        // x-apple.systempreferences:com.apple.Siri-Settings.extension
    }
}
```

`platform_check()` answers "macOS 26 on Apple silicon?" without starting the
bridge or a compiler.

## Requests that must not be replayed automatically

Use `Bridge::request_no_retry` or `request_no_retry_with_timeout` when a lost
response must not trigger another physical request. The client can replace a
dead idle connection before submission. After writing begins, write/flush
failures and disconnected responses close and join the owned process, then
return the error without reconnecting or resending. Successful calls keep the
warm connection. A subsequent explicit API call is a new submission; the client
does not deduplicate requests or reconcile uncertain outcomes.

`request` and `request_with_timeout` reconnect after a write error or lost
response, so they can send a request twice. They do not provide at-most-once
submission.

`Options::max_pending = 1` rejects concurrent calls with `QueueFull` instead of
queuing behind an in-flight request. No-retry calls use one I/O deadline for
stdin writes and response waiting. Queue waiting and process spawn remain
outside that deadline. `request` and `request_with_timeout` apply their timeout
to the response wait only.
The no-retry API does not claim a total wall-clock deadline or prove that a
request with an uncertain result did not execute. Hosts must retain uncertainty
and must not automatically call again after such an error.

A serialized request, including its schema, may be at most 1 MiB, the bridge's
line limit (excluding the newline). A larger request fails locally before any
bytes are written. Response readers stop after at most 4 MiB + 1 bytes without
waiting for a newline, so a runaway line cannot use unbounded client memory. On
Unix the bridge's stdin is a socket, so a no-retry write can time out safely.

CI runs formatting, Clippy, build, and tests with Rust 1.85.0 on macOS 14 and
Ubuntu 24.04. Fake-bridge protocol tests run on macOS without a model. Linux
checks compilation and the unsupported-platform guards; it does not claim
Foundation Models availability.

## License

MIT or Apache-2.0, at your option.
