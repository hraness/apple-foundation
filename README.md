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
apple-foundation = { git = "https://github.com/hraness/apple-foundation", tag = "v0.1.0" }
```

and resolve the bridge binary their own way (installed path, env var, or
`ensure_bridge`). Other languages can drive the executable's line protocol
directly; keep the protocol version-free and additive-only.
