# Upstream sync

Melon product lives on `main`. That branch auto-merges
[`deepseek-ai/deepseek-harness`](https://github.com/deepseek-ai/deepseek-harness)
`master` daily and on `workflow_dispatch`.

`.github/workflows/upstream-sync.yml` must remain on `main` so the schedule
keeps running after `main` is the default branch.

## Branch model

| Branch | Role |
|---|---|
| `main` | Melon Desktop product. Takes upstream `master`, then keeps Tauri, docs, and payload work. |
| `master` | Historical default from the fork. Not the product branch. Do not land Melon work here. |
| `feat/melon-desktop` | Historical feature name. Prefer `main`. |

## What the workflow does

1. Fetches `deepseek-ai/deepseek-harness:master`.
2. Merges it into Melon `main` with `--no-ff --no-verify` and pushes `main` when the merge is clean.
3. On conflict, does not push and opens or updates one `upstream-sync-conflict` issue.

Preserve Melon seams on conflict (`apps/melon-desktop/**`, brand files, README pair, `AGENTS.md` Melon section). Keep `scripts/doc-budgets.manifest.json` `AGENTS.md` at **2100**. Do not bump `runtime-pins.json` from a sync merge.

## Local dry run

```sh
git fetch upstream master
git fetch origin main
git worktree add .worktrees/sync-main origin/main
git -C .worktrees/sync-main merge --no-ff --no-verify upstream/master
```
