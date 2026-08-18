# Upstream sync

The Melon fork pulls regular updates from
[`deepseek-ai/deepseek-harness`](https://github.com/deepseek-ai/deepseek-harness)
without ever writing its default branch or auto-merging. The
`.github/workflows/upstream-sync.yml` workflow handles this daily and on
manual dispatch.

## What the workflow does

1. Fetches `deepseek-ai/deepseek-harness:master` and records its SHA.
2. Checks out the generated branch `sync/upstream-master`. On the first run
   it is cut from `master`; on later runs it is fast-forwarded to the
   remote tip.
3. Attempts `git merge --no-ff upstream/master` into that branch.
4. On success, pushes only `sync/upstream-master` and opens or updates one
   pull request against `master`. The PR description records the upstream
   SHA. The workflow never modifies `master`, never pushes tags, and never
   enables auto-merge.
5. On conflict, leaves `master` untouched, abandons the merge state, and
   opens or updates one issue with the failing upstream SHA and the
   list of conflicting files. The `upstream-sync-conflict` label makes the
   issue easy to find.

Permissions are scoped to the job (`contents: write`,
`pull-requests: write`, `issues: write`) so a leak there can only touch
the sync branch and the conflict issue.

## Manual conflict resolution

When the workflow files a conflict issue, the goal is to land
`upstream/master` cleanly into `sync/upstream-master` so the PR can be
reviewed and merged by hand. The conflict usually means upstream moved a
file that Melon also edits. Use this order:

1. **Preserve upstream runtime first.** Anything that ships under
   `packages/`, the Cordis bootstrap, the CLI, and the Web frontend
   default to upstream's version. Melon only re-bridges the runtime
   composition at the seams listed below; it does not extend, rewrap,
   or restyle the upstream runtime itself.
2. **Reapply Melon at the listed seams only.** Re-apply Melon changes to:
   - `apps/web/index.html`, `apps/web/public/manifest.webmanifest`,
     `apps/web/public/favicon.svg`
   - `packages/client/ui-primitives/src/BrandWordmark.tsx`
   - `packages/client/ui-primitives/src/FishLogo.tsx`
   - `packages/client/ui-settings-models/src/onboarding-copy.ts`
   - `packages/client/web/src/base.css` (one import of `melon-theme.css`)
   - `README.md`, `README.zh.md` (Melon quick start + attribution)
   - `AGENTS.md` (Melon downstream section, under the existing rules)
   - `apps/melon-desktop/**` and Melon-only scripts, workflows, and
     runtime pins.
   Resist the temptation to "fix" an upstream change anywhere else. If
   the conflict is outside these seams, prefer upstream and raise a new
   issue describing the divergence; do not invent a parallel convention.
3. **Update the verified-path ledger.** If upstream moved a file Melon
   edits (for example, a brand asset moved or a CSS file was renamed),
   update the seam list above in this document and in the Melon plan
   before resolving. Future runs will fail in the same place otherwise.
4. **Rerun the Gauntlet on visible changes.** Any change to the Web
   frontend, the brand surface, or the setup UI must re-enter the
   `apps/melon-desktop/design/gauntlet/` loop. Visual judgment comes
   from the running Tauri build, not from mocks. The Gauntlet verdict
   must be re-attached to the PR before merge.
5. **Push the branch and close the conflict issue.** Once `sync/upstream-master`
   merges cleanly locally and CI is green, push it. Re-run the workflow
   (or close and reopen it) to clear the conflict issue once the PR is
   ready for review.

## Verifying the workflow

The workflow is read-only against the rest of the repository. A dry run
of the merge step locally is the cheapest pre-flight:

```sh
git fetch upstream master
git worktree add -b sync/upstream-master .worktrees/sync-upstream-master origin/master
git -C .worktrees/sync-upstream-master merge --no-ff --no-verify upstream/master
```

Do this in a separate worktree. Never check the sync branch out on top of
the primary `master` checkout. `--no-verify` skips leftover lefthook
hooks that need `tsx` from `node_modules`.

2026-08-18 dry run: `origin/master` `47f943859b` merged `upstream/master`
`99f6f02fec` (`dsh` 0.1.0-rc.7, 111 commits) with zero conflicts.
`master` stayed at `47f943859b`. Melon feature work still lives on
`feat/melon-desktop` and is not in this merge. Runtime pins stay on
staged `0.1.0-rc.5` until Harness is restaged.

If that dry run merges cleanly, the workflow will too. If it conflicts,
resolve per the steps above before re-running.
