# ADO — Release / payload / seed blockers

## Goal

Record what Phase 5–7 still cannot claim, so a later slice does not invent a payload layout or ship unsigned installers from a fail-closed seed.

## Files

This ADO only. No installer, payload, or workflow mutation.

## What is 100% done vs plan

- Runtime pins exist at `apps/melon-desktop/runtime/runtime-pins.json` (DurinDoor 3.15.2, Node 20.20.2 + Node 24.19.0 sidecar).
- Staging scripts exist: `scripts/melon/stage-harness-runtime.mts`, `stage-node-sidecar.mts`, `build-durindoor-payload.mts`, `build-runtime-manifest.mts`.
- Seed installer still fail-closed before destination mutation.
- Payload descriptor keeps `managedLaunchReady: false`.
- Activate launches staged `dsh web` when sidecar + harness descriptor exist on disk (gitignored). Missing paths stay `NotImplemented` / `MissingSidecar`.

## What is blocked

- **Safe non-destructive runtime-seed installer.** Linux real-DB gate ignored/unwired. Windows/macOS verifier unsupported.
- **Windows-native CI** for archive safety, activation, ProcessTree Job Objects, controller Job lifecycle.
- **Phase 5 native payload artifacts** (unshare/sandbox) and immutable `melon-v*` release URLs.
- **Free-port managed install/start** until seed + exact Node/CLI relative paths exist. Do not invent them.
- **Unsigned installers** for Linux x64, macOS x64/arm64, Windows x64. No GitHub release `melon-v0.1.0`.
- **Gauntlet viewport win.** Round 1 judged Vite, not Tauri.
- **Phase 6 dry-run** of `upstream-sync.yml` against GitHub (workflow written, not executed).
- **Phase 7** full `check:ci` + native installer matrix + real-chat credential smoke.

## Evidence commands

```sh
# descriptor still refuse managed launch
python3 - <<'PY'
import json
from pathlib import Path
p = Path('apps/melon-desktop/runtime/runtime-pins.json')
print(p.exists(), p.read_text()[:200] if p.exists() else 'missing')
PY
```

## Not in this slice

- No payload zip, no SHA, no installer, no release tag.
- Do not claim managed-local complete.
