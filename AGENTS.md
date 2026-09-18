# Contents

- `native/AppleBridge.swift` — the product-neutral Foundation Models bridge:
  availability check, JSON-schema-subset → `DynamicGenerationSchema`
  translation, guided and bounded free-text generation, persistent NDJSON
  serve mode. Builds with `xcrun swiftc` on macOS 26+ Apple Silicon.
- `src/lib.rs` — the `apple-foundation` Rust client: lazy spawn, serialized
  in-flight request with bounded pending queue, per-request timeout,
  kill-and-respawn, `--check`/`--schema-check` helpers, `ensure_bridge`
  compiling the embedded `SWIFT_SOURCE`.
- `spec/protocol.md` — the wire contract (modes, request/response
  envelopes, bounds, error codes). Additive-only.
- `tests/client.rs` — protocol/client tests against
  `examples/fake_bridge.rs` (no model needed). Live-model paths are covered
  by `--check` and consumer smoke tests, not mocked.

# Guidelines

- Keep this crate product-neutral. It owns the session, schema translation,
  and byte bounds only — never product prompts, provider envelopes, paths,
  credentials, or command names. Product adapters live in each consumer's
  repository.
- One process, one in-flight generation: the on-device model is serial.
  Backpressure is `QueueFull`, not silent queue growth. New fields need a
  bound and a test.
- A schema the bridge cannot translate fails the request; never silently
  downgrade to free text. Hosts decide whether to retry unguided.
- Serve mode creates a fresh `LanguageModelSession` per request — no
  transcript accumulates between requests.
- Consumers pin this crate by immutable tag through a Cargo git dependency.
  Bump `Cargo.toml` version and tag `v*` for releases; keep tags immutable.
- Native checks: `cargo build`, `cargo test`,
  `cargo clippy --all-targets -- -D warnings`, `cargo fmt -- --check`, plus
  `sh scripts/build-bridge.sh` and a `--check` run on Apple Silicon.
  Compilation alone is not evidence inference works.
