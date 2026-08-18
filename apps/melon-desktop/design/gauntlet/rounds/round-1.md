# Gauntlet round 1 — 2026-08-17

## Surface judged

Live Vite setup at `http://127.0.0.1:1420/` after a fail-soft so missing Tauri `invoke` stays on choice. Not a Tauri window. OMP browser daemon was down (`omp.browser.headless` exit 21); Chromium headless + CDP captured the frames.

## Frames

| State | 1440×960 | 1024×768 |
|---|---|---|
| Choice | `candidates/choice-1440x960.png` | `candidates/choice-1024x768.png` |
| External | `candidates/external-1440x960.png` | `candidates/external-1024x768.png` |
| Insecure HTTP | `candidates/insecure-http-1440x960.png` | `candidates/insecure-http-1024x768.png` |

DOM text on choice: `Choose how Melon reaches DurinDoor` / `Install DurinDoor locally` / `Connect an existing DurinDoor` / `Built on DeepSeek Harness`. Insecure HTTP: `Traffic is not encrypted. An API key can be exposed to the network.`

## Verdict

**Not a viewport A/B win.** Tokens and hierarchy match Modern Relay. Plan checkpoint requires the running Tauri app. CSS + Vite frames cannot award either viewport.

## Largest remaining gap

Real Tauri screenshots at both viewports, then a blind A/B against `design/reference/modern-relay-*.png`.
