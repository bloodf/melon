# ADO — Gauntlet round 1

## Goal

Capture real screenshots of the live Melon setup surface at 1440×960 and 1024×768, put them on the Gauntlet ledger, and record an honest win/not-win.

## Files

- `apps/melon-desktop/design/gauntlet/candidates/choice-1440x960.png`
- `apps/melon-desktop/design/gauntlet/candidates/choice-1024x768.png`
- `apps/melon-desktop/design/gauntlet/candidates/external-1440x960.png`
- `apps/melon-desktop/design/gauntlet/candidates/external-1024x768.png`
- `apps/melon-desktop/design/gauntlet/candidates/insecure-http-1440x960.png`
- `apps/melon-desktop/design/gauntlet/candidates/insecure-http-1024x768.png`
- `apps/melon-desktop/design/gauntlet/index.html`
- `apps/melon-desktop/design/gauntlet/rounds/round-1.md`
- `apps/melon-desktop/src/App.tsx` — missing-IPC status catch stays on choice (browser preview)
- `apps/melon-desktop/src/App.test.tsx` — `stays on choice when controller status is unavailable`

## What is 100% done vs plan

- Live Vite setup served on `127.0.0.1:1420` (`melon-vite`).
- Headless Chromium CDP captured choice, external form, and insecure-HTTP confirm at both viewports. DOM text confirmed before write.
- Ledger shows randomized A/B labels (Vite = A, Modern Relay = B) plus critic verdict and next gap.
- Focused App tests: 32/32 including the new fail-soft case.

## What is blocked

- This is **Vite, not Tauri**. `pnpm run melon:dev` was not captured. OMP browser daemon failed (exit 21).
- **Not a viewport A/B win.** Plan Phase 4 checkpoint requires the running Tauri app.
- Error-state screenshot of a real probe failure was not taken (would need a live fake DurinDoor + probe IPC).
- Ports 4317/4318 remain occupied by unknown listeners from earlier Gauntlet serve attempts.

## Evidence commands

```sh
curl -sS -o /dev/null -w '%{http_code}\n' http://127.0.0.1:1420/
# 200

# CDP dump of choice text:
# "Choose how Melon reaches DurinDoor" / "Connect an existing DurinDoor"

cd apps/melon-desktop && pnpm exec vitest run src/App.test.tsx
# Test Files  1 passed (1) / Tests  32 passed (32)
```

## Not in this slice

- No Tauri window, no signed installer, no managed-local start.
- Controller failure variants owned by the controller slice.
