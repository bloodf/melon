# Upstream sync

Melon `master` tracks [`deepseek-ai/deepseek-harness`](https://github.com/deepseek-ai/deepseek-harness) `master` automatically. Melon product work lives on `feat/melon-desktop`. Do not land Tauri, branding, or payload work on `master`.

`.github/workflows/upstream-sync.yml` runs daily at 06:00 UTC and on `workflow_dispatch`.

## What the workflow does

1. Fetches `deepseek-ai/deepseek-harness:master`.
2. Merges it into Melon `master` with `--no-ff --no-verify` and pushes `master` when the merge is clean.
3. Merges that `master` into `feat/melon-desktop` and pushes the feature branch when that merge is clean.
4. On conflict, does not push the failing branch and opens or updates one `upstream-sync-conflict` issue.

This workflow file must remain on `master`. A fast-forward of Melon `master` onto the bare upstream tip would delete it and stop future syncs.

## Branch model

| Branch | Role |
|---|---|
| `master` | Upstream tracker plus this workflow. No Melon product commits. |
| `feat/melon-desktop` | Melon Desktop improvements. Takes `master`, then adds Tauri, docs, and payload work. |
| `sync/upstream-master` | Historical one-shot from the first dry run. Unused by the current action. |

## Manual conflict resolution

1. **Preserve upstream runtime first** under `packages/`, Cordis, CLI, and the Web frontend.
2. **Reapply Melon only on `feat/melon-desktop`**, at:
   - `apps/web/index.html`, `apps/web/public/manifest.webmanifest`, `apps/web/public/favicon.svg`
   - `packages/client/ui-primitives/src/BrandWordmark.tsx`
   - `packages/client/ui-primitives/src/FishLogo.tsx`
   - `packages/client/ui-settings-models/src/onboarding-copy.ts`
   - `packages/client/web/src/base.css` (one import of `melon-theme.css`)
   - `README.md`, `README.zh.md`
   - `AGENTS.md` (Melon downstream section)
   - `apps/melon-desktop/**`, Melon scripts, and runtime pins
3. Keep `scripts/doc-budgets.manifest.json` `AGENTS.md` at **2100** when the Melon section is present.
4. Rerun the Gauntlet on visible UI changes.
5. Do not bump `runtime-pins.json` from a sync merge. Restage Harness first.

## Local dry run

```sh
git fetch upstream master
git fetch origin master
git worktree add .worktrees/sync-master origin/master
git -C .worktrees/sync-master merge --no-ff --no-verify upstream/master
```

Do not check this out on top of a dirty `feat/melon-desktop` tree. `--no-verify` skips leftover lefthook hooks that need `tsx`.

2026-08-18: first dry run merged `99f6f02fec` (`dsh` 0.1.0-rc.7) with zero conflicts via PR #1. Maintainer then asked master to stay current automatically.
