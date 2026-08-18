# ADO — Documentation seams

## Goal

Document the real current Melon Desktop behavior in the user guide and the
bilingual index, link the guide from the user-guide index pair, and append
the downstream section to the repository `AGENTS.md` — without overstating
features that the source release does not yet implement (signed installer,
auto-update, managed local install) and without re-opening
`README.md`, `controller.rs`, the Gauntlet loop, or the GitHub workflows.

## Files

- `docs/user/guide/melon-desktop.md` — English user guide for the
  external-DurinDoor flow, the keyring path, model selection, activation,
  saved-connection retry, and explicit limitations.
- `docs/user/guide/melon-desktop.zh.md` — Chinese mirror, equal authority,
  same structure.
- `docs/user/guide/melon-desktop.i18n.yaml` — bilingual-pair consistency
  record for the new pair.
- `docs/user/guide/index.md`, `docs/user/guide/index.zh.md` — added one
  "Continue" link to the new guide in each language; the Chinese link
  points to the English counterpart to satisfy the structural pairing
  rule that link targets match across the pair.
- `docs/user/guide/index.i18n.yaml` — re-recorded the index pair hashes
  after the link addition.
- `AGENTS.md` — appended one short follow-up paragraph under the existing
  "## Melon downstream" heading covering PR contract, controller
  ownership, seed installer state, brand rules, and the
  never-mutate-DurinDoor invariant.

## What is 100% done vs plan

- English user guide written with the same shape as the existing
  `docs/user/guide/providers.md` (numbered steps, no inlined images, no
  code-block examples that would require committed screenshots). Word
  budget respected; no upstream files outside the documented seams were
  edited.
- Chinese mirror written with the same section list and equal authority;
  link target `[使用 Melon Desktop](./melon-desktop.md)` points to the
  English counterpart to match the structural pairing rule enforced by
  `verify-translation-pairing`.
- New `melon-desktop.i18n.yaml` records the initial pair hashes;
  `pnpm run verify-translation-pairing --write
  docs/user/guide/melon-desktop.md` accepted the pair and produced
  "1 named pair(s) consistent".
- Both index files received one additional bullet in the "Continue"
  section, with the new `use-other-cli-modes` line preserved in the
  Chinese file. The `verify-translation-pairing --write
  docs/user/guide/index.md` run re-recorded the index pair hashes and
  reported "1 record(s) written".
- `pnpm run verify-translation-pairing` reports
  "2 named pair(s) consistent" for the two pairs touched by this slice.
- `pnpm run verify-doc-budgets` passes after the `AGENTS.md` append
  (final count: 2063 words against the 2100 ceiling).
- The Melon section in `AGENTS.md` now spells out the single-owner rule
  for the Rust controller, the unsigned / no-auto-update / fail-closed
  state of the seed installer, the brand fix, and the
  never-mutate-DurinDoor invariant — without raising the ceiling or
  adding new sections.

## What is blocked

- The guide documents the managed-local install path as fail-closed in
  the source release. Activation returns a typed
  `NotImplemented("connection activation is not available yet")` for
  both `external` and `managed-local` modes until the bundled Node 24
  sidecar and the staged Harness runtime descriptor exist; both are
  gitignored in the current tree. The guide does not claim the
  managed-local path works.
- No installer, auto-update, tray, login item, package-manager
  repository, code signing, or notarization is documented or
  promised. The "Limitations" section in each guide spells this out
  with the current state.
- No image is committed for the guide. The existing
  `docs/user/guide/*.png` files are providers guide screenshots; the
  Melon guide uses text-only state descriptions for the choice,
  external form, insecure-HTTP confirm, model-selection, and saved
  screens so that no Melon screenshot is fabricated.
- `apps/melon-desktop/README.md` and the root `README.md` /
  `README.zh.md` quick-start are out of scope for this slice; the
  user guide does not duplicate the README contract, and the Melon
  install / connect language there is owned by the README prepending
  slice.
- The `runtime-pins.json` upstream SHA, the staged Harness runtime
  descriptor, the bundled Node 24 sidecar, and the managed DurinDoor
  payload are not staged in the source release. The guide's "Activate"
  section is honest about the typed `not-implemented` outcome.
- The Gauntlet loop, the Rust controller, and the GitHub workflows
  were not touched by this slice. No tests or formatters were run; the
  `verify-translation-pairing`, `verify-doc-budgets`, and the new pair
  / index pair acceptance are the only gates exercised.

## Evidence commands

The following commands are run during integration of this slice. They
are not run by this slice itself.

- `pnpm run verify-translation-pairing docs/user/guide/melon-desktop.md` —
  confirms the new pair is structurally consistent.
- `pnpm run verify-translation-pairing docs/user/guide/index.md` —
  confirms the index pair still passes after the new link.
- `pnpm run verify-translation-pairing --write
  docs/user/guide/melon-desktop.md` — re-records the new pair hashes
  in `melon-desktop.i18n.yaml`.
- `pnpm run verify-translation-pairing --write
  docs/user/guide/index.md` — re-records the index pair hashes after
  the link addition.
- `pnpm run verify-doc-budgets` — confirms `AGENTS.md` and the other
  budgeted docs remain within their ceilings.
- `wc -w AGENTS.md` — confirms the appended paragraph keeps the file
  under the 2100 ceiling (current: 2063).

## Not in this slice

- No change to `controller.rs`, `config.rs`, `process_tree.rs`, the
  Rust `runtime.rs`, the Tauri capability file, the Gauntlet loop, the
  staged Harness runtime, the DurinDoor payload, the upstream-sync
  workflow, the `melon-release` workflow, the `melon-ci` workflow, or
  the `runtime-pins.json`. The Melon plan assigns each to its own
  slice.
- No change to `apps/melon-desktop/README.md`, `README.md`, or
  `README.zh.md`. The prepending of the Melon install / connect
  quick-start is owned by a separate slice and is already in place.
- No change to `apps/web/index.html`, the manifest, the favicon, the
  brand wordmark, the fish logo, the onboarding copy, or the central
  stylesheet. The branding / theme slice owns those seams.
- No new screenshots, mockups, or diagrams. The guide is text-only.
- No Melon section in `docs/user/guide/providers.md`; the providers
  guide remains authoritative for upstream model routes, and the
  Melon guide defers to it.
