# Melon Desktop

English | [中文](README.zh.md)

Melon Desktop is the private Tauri workspace package for the installable Melon distribution of DeepSeek Harness. It keeps the upstream Cordis runtime and `@deepseek-ai/*` packages intact while owning the trusted desktop setup interface.

## Development

Run the complete package gate from the repository root:

```sh
pnpm run melon:check
```

`pnpm run melon:dev` starts the Vite setup page and Tauri application. Generated Node sidecars and staged Harness resources are ignored; packaging scripts create them from pinned, verified runtime inputs.

## Trust model

Tauri starts a disposable window labeled `setup`. Generated application permissions grant `status`, `probe`, `activate`, and `shutdown` only to that label, and capability files contain no remote URL grants. Activation will replace the setup window with a separately created, ungranted `main` Harness window. Returning to Connection Settings will reverse that native lifecycle rather than navigate the privileged webview.

The four command handlers are registered. `status` reports the current empty controller state, `shutdown` is safe before process ownership lands, and `probe` plus `activate` return typed not-implemented errors until Phase 2 supplies network, persistence, and process behavior. The setup UI calls the controller adapter for status, probing, activation, and explicit reconfiguration. Only native final app exit owns automatic shutdown; React unmount and an interceptable exit request do not stop children.

## Limitations

This package is desktop-only. It does not replace DeepSeek Harness settings, sessions, tools, permissions, or agent behavior, and it does not grant Tauri commands to loopback or remote Harness content.
