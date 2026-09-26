# Apple bridge wire protocol

The bridge (`native/AppleBridge.swift`) is a single executable wrapping
Apple's on-device Foundation Models framework (`LanguageModelSession` over
`SystemLanguageModel.default`). It is product-neutral: the host supplies
prompts, instructions, and output schemas; the bridge enforces bounds and
returns structured responses. Inference is on-device only — the bridge never
calls a network provider and never falls back to one.

## Modes

| argv             | stdin              | behavior                                              |
| ---------------- | ------------------ | ----------------------------------------------------- |
| `--check`        | ignored            | One availability JSON line on stdout, exit 0.          |
| `--schema-check` | one JSON schema    | `{"ok":true}` on stdout, or `{"ok":false,"error":{...}}` on stderr with exit 1. |
| `--once`         | one request object | One response envelope on stdout.                      |
| *(none)*         | NDJSON requests    | Serve mode: one response line per request line, in order, until EOF. |
| `--help`, `-h`   | ignored            | Human help on stdout, exit 0. Works on any macOS version. |
| `--version`, `-V`| ignored            | `<name> <version>` on stdout, exit 0.                  |

Serve mode is the preferred integration: one warm process amortizes model
startup across many requests, and the in-order line protocol serializes work
naturally. Each request gets a **fresh** `LanguageModelSession` — requests are
independent; no transcript accumulates between them.

## Availability envelope (`--check`)

```json
{"available": true, "provider": "apple", "model": "system", "onDevice": true}
{"available": false, "provider": "apple", "reason": "modelNotReady", "onDevice": true}
{"available": false, "reason": "requiresMacOS26"}
```

Reasons: `deviceNotEligible`, `appleIntelligenceNotEnabled`, `modelNotReady`,
`unavailable`, `requiresMacOS26`. The Rust client adds `helperMissing` when
the bridge executable does not exist, and reports `requiresMacOS26` or
`deviceNotEligible` itself when a bridge built for macOS 26 on Apple silicon
cannot start. Clients treat unknown reasons as `unavailable`.

## Request object

```json
{
  "id": 7,
  "prompt": "...",
  "instructions": "...",
  "schema": {"type": "object", "properties": {...}, "required": [...]},
  "expectJson": true,
  "maxOutputBytes": 16384
}
```

- `id` — string or integer, optional. Echoed back in the response. Requests
  with a missing/invalid `id` get `id: null` in the error line.
- `prompt` — required, 1..=32768 UTF-8 bytes.
- `instructions` — optional session instructions, <=4096 bytes. A neutral
  default is used when absent.
- `schema` — optional JSON-schema subset for guided generation. Supported:
  `type` of `string` (optional `pattern`, <=256-byte regex), `number`,
  `integer`, `boolean`, `array` (required `items`, max 64 elements),
  `object` (`properties` <=32, `required` subset of keys), `enum` (1..=32
  string choices). Nesting depth <=4. A schema the bridge cannot translate
  **fails the request** (`schemaDepthExceeded`, `invalidEnum`,
  `invalidPattern`, `arraySchemaRequiresItems`, `tooManyProperties`,
  `requiredPropertyMissing`, `unsupportedSchemaType`) — it never silently
  downgrades to free text. The host decides whether to retry unguided.
- `expectJson` — with no `schema`, parse the free-text response into a JSON
  value; failure is `invalidGeneratedJSON`.
- `maxOutputBytes` — optional, 1..=262144, default 16384. Generation is
  additionally capped at `min(2048, maxOutputBytes/4)` response tokens.

## Response envelope

```json
{"id": 7, "ok": true,  "value": <JSON value>}
{"id": 7, "ok": false, "error": {"code": "modelUnavailable", "reason": "modelNotReady"}}
```

Guided responses return the generated value (unwrapped from the internal
`{"value": ...}` root). Unguided responses return the text as a JSON string,
or the parsed JSON value when `expectJson` was set. Output exceeding
`maxOutputBytes` fails with `outputBudgetExceeded`.

## Bounds and failure handling

- One request line <= 1 MiB; an oversized line emits `inputBudgetExceeded`
  with `id: null` and the bridge resynchronizes at the next newline.
- `invalidRequest` covers malformed JSON, missing/oversized prompt,
  oversized instructions, bad `id`, or out-of-range `maxOutputBytes`.
- `modelUnavailable` when the system model is not usable at request time.
  Since 0.2.0 the error also carries `reason`, one of the availability
  reasons above. Older bridges omit it; clients treat a missing or unknown
  reason as `unavailable`.
- `invalidGeneratedOutput` / `invalidGeneratedJSON` /
  `outputBudgetExceeded` / `generationFailed` for generation failures.
- Unknown argv fails `unknownArguments`; fatal errors go to stderr as
  `{"ok": false, "error": {"code": ...}}` with exit 1. When stderr is a
  terminal, unknown argv instead prints a one-line human error and the help
  command, and exits 2. In serve mode with a terminal on stdin and stderr,
  the bridge prints one waiting hint to stderr; stdout stays protocol-only.

## Client contract (Rust crate)

- `Bridge::new(argv)` / `with_options(argv, Options)` — lazy spawn; requests
  are serialized through one in-flight slot (the on-device model is serial
  anyway) with `max_pending` queued callers before `QueueFull`.
- `request_timeout` per request; a timeout or malformed stream kills the
  process and the next request respawns it.
- `check(argv)` parses `--check` into `Availability { available, reason }`
  with a typed `Reason`; every known unavailable state is `Ok`, not an error.
  `Reason::explain()` returns the plain-language copy and System Settings
  link hosts show. `platform_check()` answers macOS 26 / Apple silicon
  without starting anything. Request-time `modelUnavailable` becomes
  `Error::Unavailable(reason)`.
- `schema_check(argv, schema)` validates a
  schema without generating; `ensure_bridge(path)` compiles the embedded
  `SWIFT_SOURCE` via `xcrun swiftc` when the binary is absent or stale. It
  asks `/usr/bin/xcode-select -p` first and returns `Error::ToolsMissing`
  without running `xcrun` when the tools are missing, so the install dialog
  never appears unannounced. Compiler output is captured; a failed build
  returns `Error::BuildFailed { status, log_tail }`.
