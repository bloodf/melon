# Agent Note: Melon desktop shell

Status: proposed

English | [中文](2026-08-14-melon-desktop-shell.zh.md)

## Problem

DeepSeek Harness has no installable desktop distribution, and a user currently needs a compatible Node and pnpm environment plus manual provider configuration before using it with DurinDoor. A downstream desktop product must remove those host prerequisites without replacing the Cordis runtime or making upstream synchronization depend on a copied source tree.

The setup surface needs native access to credentials, downloads, files, and child processes. The loopback Harness page is less trusted and must not inherit those capabilities. Process discovery also cannot establish ownership: Melon may connect to an existing DurinDoor, but it may stop only process trees started through handles held by the current app session.

## Proposal

Melon remains an additive fork of `deepseek-ai/deepseek-harness`. It retains the `master` branch, all `@deepseek-ai/*` package identities, the Cordis plugin runtime, `@deepseek-ai/dsh-agent-loop`, settings, sessions, permissions, tools, and the upstream Web application. Product changes stay in `apps/melon-desktop`, release and synchronization tooling, product-facing branding seams, and a central theme override.

A private `@bloodf/melon-desktop` Tauri v2 workspace package contains a bundled setup page and a Rust `ConnectionController`. Its narrow command interface reports status, probes managed or external DurinDoor connections, activates one selected model, and shuts down current-session children. Mutating operations serialize through controller state.

The setup page is the only webview granted generated custom-command permissions. Capability files contain no `remote.urls` grants. After activation, the same window navigates to the loopback Harness page without a Tauri command bridge. Rust owns endpoint validation, network probes, credential storage, downloads, extraction, atomic configuration, and process control.

Optional credentials use OS-native storage under service `com.bloodf.melon`; unavailable storage permits session-only use, never plaintext persistence. Cordis YAML references `MELON_DURINDOOR_API_KEY` and contains no key bytes.

Managed DurinDoor uses a verified per-target zip containing DurinDoor, its prepared production closure, and Node 20.20.2. Payload construction runs required package scripts against an isolated `DATA_DIR`; end-user installation never invokes npm. Melon starts the CLI on `127.0.0.1:20128` only after a strict free-port check because the CLI may terminate a selected-port listener. Managed data lives under Melon app data and is never deleted automatically.

Harness uses a verified Node 24.19.0 sidecar and a pnpm production-deploy closure built from source SHA `47f943859bef60e4160492346772ded9b24f765a`. The initial Melon release is `0.1.0`; DurinDoor is pinned to 3.15.2 at npm git head `1c14989f8ec6a56cce1df1bb2806e8ba885012f7`. `apps/melon-desktop/runtime/runtime-pins.json` is the machine-readable source of these pins and official checksum references.

Each binary distribution preserves the upstream MIT license and generates notices for DeepSeek Harness, DurinDoor, Node.js, Tauri, and bundled production dependencies. Release assets include these notices and checksums.

## Alternatives considered

**Separate wrapper repository.** This reduces overlap with upstream source but splits product branding, Web validation, runtime closure construction, and release evidence across repositories. The additive fork keeps those changes reviewable with the exact upstream tree they ship.

**Native rewrite of Harness.** This gives Tauri full control but duplicates the Cordis agent runtime, settings, sessions, tools, and Web behavior. Melon keeps one agent loop and treats Tauri as packaging and trusted setup infrastructure.

**Remote command grants for the Harness page.** This makes one window easier to wire but exposes credential, filesystem, download, and process commands to loopback-served content. Navigation instead crosses a hard capability split.

**PID-based child ownership.** Persisted PIDs survive controller restarts but can be stale or reused and cannot prove Melon spawned a process. Live process-tree handles in the current session are the only ownership evidence.

## Acceptance criteria

The proposal is implemented when native installers run without system Node or pnpm, both DurinDoor setup paths activate the real staged Harness, secrets never enter files or logs, only tracked process trees stop, remote Harness content cannot invoke setup commands, and target-specific payloads and installers verify against committed pins and release checksums.

The real Tauri surface must also pass the checked-in Modern Relay Gauntlet at 1440×960 and 1024×768 with WCAG 2.2 AA contrast and unchanged upstream Harness interactions.

## Risks

DurinDoor and Node pins age independently of upstream Harness. Updating them requires rebuilding native payloads, regenerating immutable hashes, rerunning lifecycle and installer checks, and preserving the last valid runtime until activation succeeds.

Tauri custom commands are not constrained merely by omitting remote URL grants. The build must generate per-command permissions through `tauri_build::AppManifest::commands` and grant them only to the local setup capability.

Upstream can move the small product-facing seams or change CLI argument parsing. Sync pull requests must prefer upstream runtime behavior, reapply Melon only at recorded seams, and rerun integration and visual evidence before merge.
