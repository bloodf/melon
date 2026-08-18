# Melon Desktop

English | [中文](README.zh.md)

Melon is the installable Tauri distribution of DeepSeek Harness. It keeps the
upstream Cordis agent/session/tool runtime and every `@deepseek-ai/*` package
intact and adds a trusted first-run surface that connects to an existing
DurinDoor URL, or will install a Melon-managed DurinDoor once the signed
payload installer ships. Harness runs from the upstream pnpm build under a
bundled Node 24 sidecar; DurinDoor is the initial/default model route.

## Quick start

From this checkout (developer preview — unsigned installers are not published
yet):

```sh
pnpm install
pnpm run melon:dev
```

On first launch, **Connect an existing DurinDoor**: paste the origin (or `/v1`
URL) and an optional API key. A 401 makes the key required; a keyless DurinDoor
works with the URL alone. Non-loopback `http://` URLs ask for an explicit
insecure-HTTP confirmation before any request. A provided API key is stored in
the OS-native credential store, or kept in session memory if the keyring is
unavailable; it never enters the on-disk configuration or logs.

**Install DurinDoor locally** is visible in setup but still fail-closed: the
safe payload installer and `managedLaunchReady` descriptor are not shipped.
Do not expect a download or a start on `127.0.0.1:20128` from this tree.

## Attribution

Melon is built **on DeepSeek Harness**. DeepSeek Harness (`dsh`) is the
open-source agent harness developed by DeepSeek AI and is the runtime
Melon embeds and configures. All upstream behaviour, documentation, and
the MIT licence in LICENSE apply to this fork. See
THIRD_PARTY_NOTICES.md for the DurinDoor, Node, and Tauri notices
shipped with Melon.

The remaining sections below are the upstream DeepSeek Harness
documentation, retained verbatim.

---

# DeepSeek Harness

English | [中文](README.zh.md)

DeepSeek Harness (`dsh`) is an open-source agent harness developed by [DeepSeek AI](https://deepseek.com).

It uses an architecture where **everything is a plugin**, and is powered by [Cordis](https://github.com/cordiverse/cordis), whose design is described in [_A Programming Paradigm for Spatiotemporal Composability_](https://github.com/cordiverse/paper).

## Developer preview

DeepSeek Harness is currently in _developer preview_ and is iterating rapidly. **THERE WILL BE COMPATIBILITY-BREAKING CHANGES.**

## Run

### Run from `npm`

Install `Node.js`, then run:

```sh
npx @deepseek-ai/dsh web
```

The command starts the Web UI, served at `http://127.0.0.1:3080` by default. See [Web UI guide](docs/user/guide/index.md).

### Run from source

To run from a repository checkout:

```sh
git clone https://github.com/deepseek-ai/deepseek-harness.git
cd deepseek-harness
pnpm install
pnpm run build
pnpm dsh web
```

## Community and support

- Feel free to submit feedback or bug reports through [GitHub Discussions](https://github.com/deepseek-ai/deepseek-harness/discussions).
- Add the [`dsh-plugin`](https://github.com/topics/dsh-plugin) topic to your plugin repository for discoverability.
- Join <a href="https://discord.gg/Ycq5dCaS4">DeepSeek Harness Discord community</a>.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## Development

Start with the [development guide](docs/development.md) and [architecture documentation](docs/architecture.md).

For agents, follow [AGENTS.md](AGENTS.md).

## License

[MIT](LICENSE)

Third-party dependencies and their licenses are disclosed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
