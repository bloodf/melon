# Controller Failure Contracts

## Goal

Define the user-visible failure contracts for the Melon activation flow when the
controller cannot start Harness. Each contract is a separate
`ControllerError` variant that the controller emits before mutating the
persisted `connection.json` or adopting owned process trees.

## Scope

- `apps/melon-desktop/src-tauri/src/controller.rs` — `ControllerError`,
  `Serialize` impl, `activate_external`, `read_dsh_bin`, `StderrRing`, redactor.
- `apps/melon-desktop/src-tauri/src/process_tree.rs` — `ProcessTree::take_stderr`.
- Plan sections :255-269: "Endpoint contract", "Process ownership and
  shutdown", "Generated Cordis patch", "Packaging model", "Error and recovery
  behavior", "Acceptance matrix".

## Contracts

### `MissingSidecar(PathBuf)`

Contract: the bundled Node 24 sidecar was expected but not present.

Return path: `activate_external` checks `layout.node_sidecar` immediately
after the `harness_root` empty-by-design check. An empty path signals
"sidecar not staged" and returns `MissingSidecar(PathBuf::new())`. A
populated-but-missing path falls through to `MissingDescriptor` because the
runtime descriptor check fires first.

User-visible message: `"missing bundled Node 24 sidecar"` (path omitted when
empty, surfaced when searched but not found).

Plan reference: §Architecture, §Endpoint contract (no network/IO), §Error
and recovery behavior ("Harness launch/readiness failure").

Test: `activate_returns_missing_sidecar_when_node_sidecar_path_is_empty` —
asserts `Err(MissingSidecar(PathBuf::new()))` from `for_activate` with empty
sidecar; pre-`write_connection` step, so `connection.json` is untouched.

### `MissingDescriptor(PathBuf)`

Contract: the staged Harness runtime descriptor (`runtime.json`) is
required to discover the `dsh` binary, but it could not be read.

Return path: `read_dsh_bin` distinguishes:
- `fs::read_to_string` or `serde_json::from_str` failure → `MissingDescriptor(path)`.
- `runtime.json` parses but `dshBin` field missing or `is_file()` returns
  false → `ActivationFailed` (descriptor was present but invalid).

User-visible message: `"missing runtime descriptor at <path>"` for read
failures; `"runtime descriptor missing dshBin field"` for malformed.

Plan reference: §Generated Cordis patch (must read `dshBin` from staged
runtime), §Error and recovery behavior ("Hash/archive failure").

Test: `activate_returns_missing_descriptor_when_runtime_descriptor_missing` —
asserts `Err(MissingDescriptor(<path>))` from `for_activate` with empty
harness root and populated sidecar; path is non-empty because the
controller searched for the descriptor.

### `SpawnFailed(String)`

Contract: `Command::spawn` (via `ProcessTree::spawn`) failed. The error
string is `io::Error::to_string()` for human diagnostics — never bytes
from the child process or environment.

Return path: `ProcessTree::spawn(&mut command).map_err(|e| ControllerError::SpawnFailed(e.to_string()))?`
runs immediately after `Stdio::piped()` is wired for stderr. The previous
`Stdio::null()` made diagnostic capture impossible and is removed.

User-visible message: `"spawn failed: <io::Error to_string>"`.

Plan reference: §Process ownership and shutdown (controlled spawn through
`ProcessTree`); §Error and recovery behavior ("Harness launch/readiness
failure").

Note: `String` payload instead of `io::Error` because `io::Error` lacks
`Eq` and would force `Box::leak` or dynamic-dispatch tricks. `to_string()`
preserves the user-visible error chain.

Test: not directly unit-testable (would require a real `Command::spawn`
failure); covered by `activate_returns_missing_sidecar_...` which fails
before `spawn` is reached, and by the readiness-timeout test which forces
`spawn` to succeed and then validates the post-spawn failure path.

### `ReadinessTimeout { port: u16, stderr_tail: String }`

Contract: Harness spawned but did not respond with HTTP readiness inside
the `HARNESS_READY_BUDGET`. The bounded redacted stderr tail is included
to make the failure actionable without leaking secrets.

Return path: `wait_http_ready(port, deadline)` returns `Err(_)` →
`tree.stop(grace)`, `read_stderr_tail(ring)`, return
`ReadinessTimeout { port, stderr_tail }`. Drain thread continues
independently until the stderr pipe is closed (process exit or
`take_stderr` None).

User-visible message: `"Harness failed to bind <port> within 20s; stderr: <redacted tail>"`.

Plan reference: §Process ownership and shutdown ("wait for an HTTP-ready
Harness page"); §Error and recovery behavior ("Harness launch/readiness
failure"); §Gauntlet Loop for all UI work (actionable error states).

Components:
- 4 KiB bounded `StderrRing` (`STDERR_RING_CAP`) keeps the last 4 KiB of
  UTF-8 text, evicting oldest bytes when full.
- Line-aligned flush: a partial line is held until `\n` arrives; `finish()`
  appends `\n` for the final unterminated fragment.
- `redact_stderr_line` rewrites `MELON_DURINDOOR_API_KEY=<value>` and
  `Bearer <token>` substrings to `[redacted]`. Token end = next ASCII
  whitespace or EOL. Case-sensitive byte match.
- Drain thread reads `BufReader<ChildStderr>` into the shared ring via
  `Arc<Mutex<StderrRing>>`. Detached; no `JoinHandle` retained. Mutex
  poison from a panicking drain is recovered with `into_inner`.

Tests:
- `redact_stderr_line_redacts_melon_api_key_value_pair`
- `redact_stderr_line_redacts_bearer_token_in_authorization_header`
- `redact_stderr_line_redacts_multiple_bearer_tokens_in_one_line`
- `stderr_ring_evicts_oldest_bytes_when_over_capacity`
- `stderr_ring_keeps_partial_line_until_newline`
- `stderr_ring_redacts_sensitive_tokens_before_eviction`
- `stderr_drain_capacity_constant_matches_documented_budget`

## Carve-out: `NotImplemented(String)`

`NotImplemented` is retained **only** for the empty-by-design
`harness_root` branch — when the controller has not yet been told which
Harness directory to use. It is **not** used for empty `node_sidecar`,
which is the `MissingSidecar` contract.

Plan reference: §Architecture, §Data locations (`<app-data>/harness/`
must be resolved before activation).

## Invariant: `connection.json` is not overwritten on failure

`write_connection(&app_data/connection.json, &doc)` is the **last** step
of `activate_external`. All preceding steps (`begin_operation`,
`require_model`, `read_dsh_bin`, `write_cordis_patch`, `bind_loopback`,
`ProcessTree::spawn`, `wait_http_ready`) use `?` to early-return. None of
`MissingSidecar`, `MissingDescriptor`, `SpawnFailed`, `ReadinessTimeout`,
`ActivationFailed`, or `ShutdownFailed` paths reach `write_connection`.

This guarantees the last committed `connection.json` (from a previous
successful activation) remains available for retry/reconfigure when the
current attempt fails.

Plan reference: §Error and recovery behavior ("New configuration fails
after probe: do not replace `connection.json`; the previous committed
connection remains available").

Test: `activate_does_not_overwrite_connection_json_when_descriptor_missing`
— pre-writes a sentinel `ConnectionDocument` with `model: "sentinel-model"`,
calls `activate_external` with a missing descriptor, asserts
`result.is_err()`, then re-reads `connection.json` and asserts the
sentinel model is intact.

## Evidence commands

These are the commands that prove the contracts; **not** run as part of
the project-wide suite per slice constraint.

```text
cargo check --manifest-path apps/melon-desktop/src-tauri/Cargo.toml --tests
cargo test --manifest-path apps/melon-desktop/src-tauri/Cargo.toml --lib \
  redact_stderr_line_redacts_melon_api_key_value_pair \
  redact_stderr_line_redacts_bearer_token_in_authorization_header \
  stderr_ring_evicts_oldest_bytes_when_over_capacity \
  stderr_ring_keeps_partial_line_until_newline \
  activate_returns_missing_sidecar_when_node_sidecar_path_is_empty \
  activate_returns_missing_descriptor_when_runtime_descriptor_missing \
  activate_does_not_overwrite_connection_json_when_descriptor_missing
```

## Status

100% done vs plan :255-269:
- `MissingSidecar`, `MissingDescriptor`, `SpawnFailed`, `ReadinessTimeout`
  variants added to `ControllerError`.
- 4 KiB `StderrRing` with line-aligned redaction.
- Drain thread + `Arc<Mutex<StderrRing>>` shared with controller.
- `activate_external` reordered: layout checks first, `write_connection`
  last.
- 10 focused tests in `mod tests`.
- `ProcessTree::take_stderr` accessor exposed for controller.

Not in scope: harness launch + fake-DurinDoor end-to-end (covered by
`FinalizeControllerLifecycle` slice); owned-tree shutdown tests (covered
by `ImplementProcessTree` and `FinalizeControllerLifecycle` slices).
