# Contributing

Orchard has one shared Rust engine, a desktop UI, and a JSON CLI / stdio MCP server.

```sh
npm ci
cargo test -p orchard-core -p orchard-cli
cargo build -p orchard-cli
npm run test:mcp
npm run build
cargo fmt --all --check
```

The MCP integration test uses the official client SDK and a temporary database. It makes no Apple requests. Node 22.13+ is required for its SQLite fixture setup.

Use minimal captured fixtures for catalog or endpoint changes. Keep failed checks distinct from out-of-stock results. Never fabricate history, copy browser credentials, or disable request pacing. A fallback must preserve cooldowns and Apple's Retry-After. Tests should not send live Apple requests by default.

Open an issue with the version, device ID, endpoint, status code, and a redacted diagnostic export. Do not attach your inventory database, cookies, tokens, or personal browser profile.

Desktop development needs [Tauri's platform prerequisites](https://v2.tauri.app/start/prerequisites/). The CLI needs only Rust and a C toolchain for bundled SQLite. All commands run from the repository root.
