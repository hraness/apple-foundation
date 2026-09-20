# apple-foundation

Product-neutral access to Apple's on-device Foundation Models framework for
Rust (and other) hosts. Free, private, local inference on macOS 26+ Apple
Silicon — no API key, no network egress, no fallback to a cloud provider.

Two pieces:

- `native/AppleBridge.swift` — a small executable that wraps
  `LanguageModelSession`. Speaks a bounded NDJSON protocol (one request line
  in, one response line out, in order) so a single warm process serializes
  generation naturally. Guided output uses `DynamicGenerationSchema`
  translated from a JSON-schema subset, so structured responses are
  schema-constrained rather than prompt-policed. Modes: `--check`
  (availability), `--schema-check`, `--once`, and persistent serve mode.
- `src/lib.rs` — the `apple-foundation` Rust client: lazy spawn, one
  in-flight request with a bounded pending queue (`QueueFull` backpressure),
  per-request timeout, kill-and-respawn on failure, `ensure_bridge()` to
  compile the embedded Swift source when no binary is installed.

Wire contract: [`spec/protocol.md`](spec/protocol.md).

## Build

```sh
sh scripts/build-bridge.sh            # → target/debug/apple-bridge
target/debug/apple-bridge --check     # {"available":true,...} on macOS 26+
cargo test                            # protocol + client tests (fake bridge)
```

Compilation alone is not evidence the model works — run `--check` for live
availability (`modelNotReady`, `appleIntelligenceNotEnabled`,
`deviceNotEligible` are real states).

## Consumers

Rust hosts depend on the crate by immutable tag:

```toml
apple-foundation = { git = "https://github.com/hraness/apple-foundation", tag = "v0.1.3" }
```

and resolve the bridge binary their own way (installed path, env var, or
`ensure_bridge`). Other languages can drive the executable's line protocol
directly; keep the protocol version-free and additive-only.

## Requests that must not be replayed automatically

Use `Bridge::request_no_retry` or `request_no_retry_with_timeout` when a lost
response must not trigger another physical request. The client can replace a
dead idle connection before submission. After writing begins, write/flush
failures and disconnected responses close and join the owned process, then
return the error without reconnecting or resending. Successful calls keep the
warm connection. A subsequent explicit API call is a new submission; the client
does not deduplicate requests or reconcile uncertain outcomes.

`request` and `request_with_timeout` retain their existing reconnect behavior:
a write error or lost response may resend a request. They do not provide
at-most-once submission.

`Options::max_pending = 1` rejects concurrent calls with `QueueFull` instead of
queuing behind an in-flight request. No-retry calls use one I/O deadline for
stdin writes and response waiting. Queue waiting and process spawn remain
outside that deadline. Legacy calls retain their response-only timeout.
The no-retry API does not claim a total wall-clock deadline or prove that a
request with an uncertain result did not execute. Hosts must retain uncertainty
and must not automatically call again after such an error.

Request serialization is bounded to the bridge's existing 1 MiB line limit
(excluding the newline), including schemas; oversized requests now fail locally
before any request bytes are written. Response readers stop after at most
4 MiB + 1 bytes without waiting for a newline. This prevents an unbounded line
from allocating unbounded client memory. Wire fields and Swift source are
unchanged; Unix stdin uses a socket so strict delivery can time out safely.

CI runs formatting, Clippy, build, and tests with Rust 1.85.0 on macOS 14 and
Ubuntu 24.04. Fake-bridge protocol tests run on macOS without a model. Linux
checks compilation and the unsupported-platform guards; it does not claim
Foundation Models availability.
