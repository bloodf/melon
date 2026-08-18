# Use Melon Desktop

English | [中文](melon-desktop.zh.md)

Melon Desktop is the installable Tauri distribution of DeepSeek Harness. It keeps the upstream Cordis runtime, `@deepseek-ai/*` packages, settings, sessions, tools, permissions, and agent behavior intact, and owns only the desktop connection surface. The Tauri window labeled `setup` is the only surface that can invoke native commands; the loopback Harness page never receives Tauri capabilities. This guide describes the current setup flow in the source release.

## Prerequisites

- Linux x64, macOS x64 or arm64, or Windows x64
- A DurinDoor endpoint at a URL you can reach, either a local `127.0.0.1` service you have started yourself or a remote URL with an optional API key

## First run

On a fresh launch Melon shows the choice screen with two options:

- **Install DurinDoor locally** — download and run a managed DurinDoor on this machine.
- **Connect an existing DurinDoor** — point Melon at a URL and optional API key.

The "Install DurinDoor locally" path is not available in the source release. Building and packaging the managed runtime is a separate step that produces the signed installer; the current source tree does not include a managed local install flow, an auto-update channel, or a signed installer. Use **Connect an existing DurinDoor** against a `127.0.0.1` URL after you have started DurinDoor yourself.

## Connect an existing DurinDoor

1. Choose **Connect an existing DurinDoor**.
2. Enter the URL. The form normalizes the URL so it ends in `/v1`. Credentials in URL userinfo, query string, or fragment, and any path after `/v1`, are rejected.
3. Leave the API key blank for a keyless DurinDoor; enter the key for an authenticated one. A 401 during the model discovery call returns you to the form and marks the key field as required.
4. If the URL is `http://` and the host is not loopback, the form asks for an explicit insecure-HTTP confirmation before any request. Loopback `http://` (`127.0.0.1`, `localhost`, `::1`, `[::1]`) does not require confirmation.
5. Submit. Melon probes the URL and loads the model catalog.

### What Melon probes

For every external connection Melon issues these requests against the normalized management URL (the URL with the trailing `/v1` removed):

- `GET <management>/api/health` when the host exposes it. A successful response confirms a DurinDoor instance; a missing health route through a reverse proxy is a warning, not a hard failure.
- `GET <management>/api/v1/realtime/auth` with the bearer key. `200` accepts the key; `401` returns to the form; `404` or `405` marks credential validation as unavailable.
- `GET <baseUrl>/models` with the same bearer (or the placeholder `sk_durindoor` for keyless). A non-empty `data` array is required.

Probing never sends a billable chat completion.

## Choose a model

The model picker lists the IDs the endpoint returned. The **Launch Melon** button stays disabled until one model is selected. If the endpoint could not verify the key, the picker labels the API key as "not verified"; selecting a model and launching still works.

A second connection, a model swap, and the upstream model picker in the Web UI all remain available after activation. Melon sets the initial DurinDoor route only.

## Activate

Activate writes a generated Cordis patch and a non-secret connection document into `<app-data>/harness/`, binds a loopback port, launches the staged `dsh --profile web` under the bundled Node 24 sidecar with `MELON_DURINDOOR_API_KEY` and `DSH_HOME` set, and waits for an HTTP 200 before returning. The setup window replaces itself with the loopback Harness page.

A provided API key persists in the OS keyring under service `com.bloodf.melon` with the normalized endpoint as the account id. Only that account id is stored in `connection.json`. If the credential service is unavailable, the key stays in session memory and the form asks again on the next launch. A later activate with no key reloads the stored secret into the child environment.

In the source release, activation requires the bundled Node 24 sidecar and the staged Harness runtime descriptor; neither is committed, and building and packaging them is a separate step. The current activate path returns a typed `not-implemented` error until those resources are staged.

## Saved connection and reconfigure

Once a connection is committed, the native **Connection Settings…** menu returns the window to the setup surface. The form shows the saved URL, model, and account id; it does not echo the API key. **Retry launch** reuses the saved connection; **Change connection** starts the choice screen again.

## Limitations

- The managed-local installation path is not available in the source release.
- The seed installer has no signed installer, no auto-update, and no managed local install flow.
- Melon never edits DurinDoor source, the `~/.9router` database, or any process it did not spawn in the current session.
- The brand is fixed: rail wordmark "Melon", the "M" mark, the footer "Built on DeepSeek Harness". Theme overrides live in [`packages/client/web/src/melon-theme.css`](../../client/web/src/melon-theme.css); no scattered color literals.
- The upstream model picker and the [model configuration guide](./providers.md) still own provider settings; Melon sets the initial DurinDoor route only.

## Development

The package gate, Vite dev server, sidecar staging, and runtime staging live in [apps/melon-desktop/README.md](../../apps/melon-desktop/README.md). Use `pnpm run melon:dev` for the Tauri surface and `pnpm run melon:check` for the focused test and contract suite.
