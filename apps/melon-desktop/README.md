# Melon Desktop

English | [中文](README.zh.md)

Melon Desktop is the private Tauri workspace package for the installable Melon distribution of DeepSeek Harness. It keeps the upstream Cordis runtime and `@deepseek-ai/*` packages intact while owning the trusted desktop setup interface.

## Development

Run the complete package gate from the repository root:

```sh
pnpm run melon:check
```

`pnpm run melon:dev` starts the Vite setup page and Tauri application. Generated Node sidecars and staged Harness resources are ignored; packaging scripts create them from pinned, verified runtime inputs.

`pnpm run melon:stage-node-sidecar -- --target <triple> --archive <official-node-archive> --checksums <SHASUMS256.txt>` verifies the official Node 24.19.0 archive against committed pins and atomically publishes `src-tauri/binaries/node-<triple>`. Run `pnpm run melon:test:node-sidecar` for the fixture contract.

`generateMelonCordisPatch` writes a non-secret `--patch` overlay that selects DurinDoor, disables native DeepSeek chat/search, and never embeds API-key bytes. Run `pnpm run melon:test:cordis-patch`.

`activate` launches staged `dsh --profile web` under the Node sidecar after a successful probe. It writes `<app-data>/harness/melon.cordis.patch.yml` with `serde_yaml`, waits for `GET /` 200, then writes `connection.json`. A provided API key is stored in the OS keyring (`com.bloodf.melon`) or session-only memory; only a native account id is persisted. A later activate with no key reloads that stored secret into `MELON_DURINDOOR_API_KEY`. Production resolve prefers bundled `node` beside the executable and `<resource_dir>/resources/harness`; missing sidecar or descriptor keeps `activate` as `NotImplemented`. Focused proof: `cargo test --manifest-path apps/melon-desktop/src-tauri/Cargo.toml activate` and `pnpm run melon:test:fake-chat`.

## Harness runtime

`pnpm run melon:stage-harness-runtime` builds the upstream packages and Web frontend, verifies the current packed-install release path, deploys the `@deepseek-ai/dsh` production closure, validates package metadata and dependencies, then atomically publishes the generated closure under `src-tauri/resources/harness/`. Its descriptor hash covers exact staged artifact bytes and executable bits, excluding the descriptor file itself; upstream bundles may embed the checkout path, so this is artifact integrity rather than cross-machine reproducibility. If publication and automatic restoration both fail, the error names a retained `.harness-backup-*/runtime` directory beside `harness`; restore that directory before deleting it. Run `pnpm run melon:test:harness-runtime` for the fixture-based staging contract.

## DurinDoor payload

Release runners build one target-native DurinDoor payload with committed CLI, shipped runtime-seed, and build-only npm locks. `runtime-pins.json` owns the DurinDoor version/package integrity, exact Node 20.20.2 runtime archives, and shared official headers archive. Builder authenticates inputs and exact build-only `node-gyp@10.1.0`, then invokes it under verified Node 20 inside the positively probed OS network sandbox. Release runner supplies absolute Python/C/C++ executables under non-world-writable allowlisted directories; only those directories enter build `PATH`. Private paths remain build-only. `payload.json` records deterministic tool basenames, normalized first-line versions, executable SHA-256 values, and a hash-bound `metadata/runtime-seed-manifest.json`; that sorted sidecar gives each locked seed file's destination-relative path, size, SHA-256, and executable bit for a future non-destructive app-data merge. It carries module paths and locked versions but no executable script or argument data. The payload remains marked `managedLaunchReady: false` until native installation consumes this authority—never PATH, realpaths, home, workspace, or user-data paths.

```sh
pnpm run build:durindoor-payload -- \
  --target x86_64-unknown-linux-gnu \
  --node-archive /path/to/node-v20.20.2-linux-x64.tar.gz \
  --headers-archive /path/to/node-v20.20.2-headers.tar.gz \
  --checksums /path/to/SHASUMS256.txt \
  --sandbox-runner /usr/bin/unshare \
  --toolchain-dir /usr/bin \
  --python /usr/bin/python3 \
  --cc /usr/bin/cc \
  --cxx /usr/bin/c++ \
  --output /path/to/melon-durindoor-x86_64-unknown-linux-gnu.zip
```

Run `pnpm run test:durindoor-payload` for authenticated runtime/header inputs, sandboxed source-build behavior, canonical ZIP, target policy, locked-version/integrity, native/WASM, license-notice, Rust activation, and durable publication coverage. Linux builds require `unshare` with working user and network namespaces; the builder probes namespace isolation before any payload work and fails closed otherwise. macOS and Windows builders remain blocked until native runners provide and prove equivalent positive network sandboxes. All targets use the pinned shared official headers archive; missing, linked, special, duplicate, or unsafe header entries fail closed rather than using ambient headers. Linux evidence cannot be represented as macOS or Windows offline proof. Payload generation does not make managed launch ready: `payload.json` records `runtimeSeedPath` and `managedLaunchReady: false`; managed start remains disabled until Rust installs missing locked seed files non-destructively and validates them with bundled Node.

## Trust model

Tauri starts a disposable window labeled `setup`. Generated application permissions grant `status`, `probe`, `activate`, and `shutdown` only to that label, and capability files contain no remote URL grants. Activation will replace the setup window with a separately created, ungranted `main` Harness window. Returning to Connection Settings will reverse that native lifecycle rather than navigate the privileged webview.

The four command handlers are registered. `status` reports the current empty controller state, `shutdown` is safe before process ownership lands, and `probe` plus `activate` return typed not-implemented errors until Phase 2 supplies network, persistence, and process behavior. The setup UI calls the controller adapter for status, probing, activation, and explicit reconfiguration. Only native final app exit owns automatic shutdown; React unmount and an interceptable exit request do not stop children.

## Limitations

This package is desktop-only. It does not replace DeepSeek Harness settings, sessions, tools, permissions, or agent behavior, and it does not grant Tauri commands to loopback or remote Harness content.
