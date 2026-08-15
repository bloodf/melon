# Melon Desktop

English | [中文](README.zh.md)

Melon Desktop is the private Tauri workspace package for the installable Melon distribution of DeepSeek Harness. It keeps the upstream Cordis runtime and `@deepseek-ai/*` packages intact while owning the trusted desktop setup interface.

## Development

Run the complete package gate from the repository root:

```sh
pnpm run melon:check
```

`pnpm run melon:dev` starts the Vite setup page and Tauri application. Generated Node sidecars and staged Harness resources are ignored; packaging scripts create them from pinned, verified runtime inputs.

## DurinDoor payload

Release runners build one target-native DurinDoor payload with committed CLI and runtime-seed npm locks. The builder verifies the official Node 20.20.2 archive checksum, installs the DurinDoor closure with scripts disabled, installs the native runtime seed with its reviewed `better-sqlite3` lifecycle enabled under an isolated `DATA_DIR`, excludes npm shims and all symlinks, validates the staged CLI offline, and writes a deterministic ZIP.

```sh
pnpm run build:durindoor-payload -- \
  --target x86_64-unknown-linux-gnu \
  --node-archive /path/to/node-v20.20.2-linux-x64.tar.gz \
  --checksums /path/to/SHASUMS256.txt \
  --output /path/to/melon-durindoor-x86_64-unknown-linux-gnu.zip
```

Run `pnpm run test:durindoor-payload` for canonical ZIP, target policy, locked-version, native/WASM, license-notice, and Rust activation fixture coverage. Native release evidence remains target-runner specific: Linux cannot prove the macOS or Windows `better-sqlite3` and tray binaries. Payload generation does not make managed launch ready: `payload.json` records `runtimeSeedPath` and `managedLaunchReady: false`; managed start must remain disabled until Rust installs missing locked seed files non-destructively and validates them with bundled Node.

## Trust model

Tauri starts a disposable window labeled `setup`. Generated application permissions grant `status`, `probe`, `activate`, and `shutdown` only to that label, and capability files contain no remote URL grants. Activation will replace the setup window with a separately created, ungranted `main` Harness window. Returning to Connection Settings will reverse that native lifecycle rather than navigate the privileged webview.

The four command handlers are registered. `status` reports the current empty controller state, `shutdown` is safe before process ownership lands, and `probe` plus `activate` return typed not-implemented errors until Phase 2 supplies network, persistence, and process behavior. The setup UI calls the controller adapter for status, probing, activation, and explicit reconfiguration. Only native final app exit owns automatic shutdown; React unmount and an interceptable exit request do not stop children.

## Limitations

This package is desktop-only. It does not replace DeepSeek Harness settings, sessions, tools, permissions, or agent behavior, and it does not grant Tauri commands to loopback or remote Harness content.
