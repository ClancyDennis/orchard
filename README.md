# Orchard

Apple pickup availability, observed over time. A local desktop app, JSON CLI, and MCP server for iPhone, iPad, and Mac shoppers.

Orchard reads Apple's public pickup endpoints, records real observations in SQLite, and shows when a configuration becomes available nearby. Failed checks remain **unknown**, not “out of stock.” It does not reserve products or place orders.

## Desktop

```sh
git clone https://github.com/ClancyDennis/orchard.git
cd orchard
npm ci
npm run desktop
```

Requires Node 22.13+, Rust stable, and [Tauri's platform prerequisites](https://v2.tauri.app/start/prerequisites/). To create a local macOS app and DMG, run `npm run bundle`. The current desktop build has been tested on Apple Silicon macOS. It is not signed or notarized for public distribution; Windows/Linux desktop packaging has not been verified.

On first launch, select a category, device, configuration, and US ZIP code. Collection starts after you save an enabled schedule. Existing Orchard installations retain their settings and history.

- **Devices:** iPhone Duo, 18 Pro / Pro Max, and other cataloged iPhones; iPad, Air, Pro, and mini; standard Mac configurations. The bundled catalog also includes Studio Displays.
- **Configuration search:** choose all configurations or one exact capacity, color, size, chip, and SKU. iPhones support unlocked or carrier variants. iPads include Wi-Fi and unlocked cellular variants.
- **Catalog refresh:** read current part numbers and dimensions from Apple's buy-page data. Failed refreshes keep the last good catalog. Add a newer US family buy page from Settings without editing source code.
- **History:** switch the History device selector between saved device/configuration/carrier/ZIP timelines without changing the active collector. The graph, log, availability cards, filters, and patterns all follow that selection. iPhone, iPad, and Mac graphs use distinct colors; CSV exports include device and tracker identity. History begins with your own observations.
- **Pickup links:** standard and education-store links for the specific configuration. Apple's checkout determines eligibility, pricing, and final pickup availability.
- **Schedules:** daily windows, independent rates, overnight support, and an IANA time zone. Closing the window keeps the collector in the tray; the app must be running and the computer awake.
- **Notifications:** the desktop app alerts on newly available store/configuration combinations. Continuous stock and failed checks do not create repeated alerts.
- **When to watch:** local statistics identify hours with observed availability, accounting for different polling rates and failure gaps. This is historical evidence, not Apple's update schedule.

The desktop currently schedules **one selected device target at a time**. Selecting another target opens its separate history. Agent clients can check other targets without changing the desktop selection; all share the same request budget when using the same database.

## CLI and MCP

The CLI needs no Node, GUI, Apple login, or API key:

```sh
cargo install --path crates/orchard-cli --locked
orchard catalog --category ipad
orchard catalog --device ipad-pro
orchard check --device ipad-pro --zip 10001 --part MDWU4LL/A
orchard watch --device ipad-pro --zip 10001 --part MDWU4LL/A --interval 120 --timeout 1800
```

`check` returns JSON; `watch` emits NDJSON and waits until stock appears or its timeout expires. Always select SKUs from `catalog`; bundled examples may become outdated. `latest`, `history`, and `status` read saved state without querying Apple.

Start the stdio MCP server with `orchard mcp`. It provides six tools for catalog discovery/refresh, inventory checks, saved inventory/history, and status. See **[agent setup and API semantics](docs/agents.md)** for Codex, Claude Code, and other MCP clients.

```sh
codex mcp add orchard -- /absolute/path/to/orchard mcp
claude mcp add --transport stdio orchard -- /absolute/path/to/orchard mcp
```

The CLI, MCP server, and desktop use the same Rust library, database format, and pacing gate. MCP opens no HTTP listener. Commands for another target do not modify desktop settings.

## Apple connections and limits

Automatic mode first uses `/shop/retail/pickup-message`. If that endpoint returns 404 or 410, it can use `/shop/fulfillment-messages` through a normal browser session. Direct-only and browser-session modes are also selectable. A browser session requires installed Chrome, Edge, or Chromium; it uses its own temporary profile and no personal Safari/Chrome credentials.

**A 541, 429, or an active Retry-After does not trigger endpoint switching.** All methods share a persisted minimum of one minute between checks. Your slower configured rate is preserved. The first 541/429 starts a fifteen-minute cooldown, increasing to thirty and sixty minutes after consecutive failures. Manual clicks, restarts, and device changes cannot shorten a cooldown. Two minutes is a reasonable starting rate; no rate guarantees Apple will accept requests, and other clients on the same public IP may share its limits.

Diagnostics record the endpoint, HTTP status, transport, timing, and optional browser cookie **metadata**. Cookie values, authentication headers, raw response bodies, and personal browser data are excluded. Logs stay local and rotate. A successful HTTP response must still contain all requested configurations for each returned store before Orchard accepts it as inventory.

Orchard uses Apple-native paths only. The old third-party collector has been removed. Legacy tracker addresses are decoded solely during migration into local `apple://us/...` identities; archived observations retain their original source label. These identities are database keys, not network URLs.

## Catalog scope

The catalog parser reads `productSelectionData` as JSON; it does not execute Apple's page scripts. iPhone/iPad pages expose sellable SKUs directly. Mac pages include standard SKU configurations and customizable entries; entries without a sellable pickup SKU are excluded. Apple product redirects supply memory/storage descriptions where available. Incomplete descriptions are labeled with the SKU and a detail warning. Custom-built Macs and every possible CTO combination are not supported.

This release is **US-only**. New product-page layouts may require a parser update. Catalog freshness and inventory freshness are separate: a newly refreshed catalog does not establish stock.

## Data and development

Data stays under `local.orchard.iphone-tracker` in the platform's application-data directory. `orchard status` or desktop Settings shows the database path. `--database /absolute/path/inventory.sqlite3` selects a separate workspace; it does not grant extra Apple quota. Existing migrations preserve snapshot IDs, timestamps, failures, and schedules.

```sh
cargo test --locked -p orchard-core -p orchard-cli
cargo build --locked -p orchard-cli
npm run test:mcp
npm run build
cargo fmt --all --check
```

- `crates/orchard-core`: catalog, request transports, SQLite, pacing, history, and analysis.
- `crates/orchard-cli`: JSON CLI and stdio MCP server.
- `src-tauri`: desktop integration and notifications.
- `src`: React UI.

Tests use fixtures and local HTTP servers. The official MCP SDK test verifies handshake, tools, historical/unknown states, cooldown sharing, and bounded polling without Apple requests. An explicitly ignored live test is available for manual endpoint verification. `npm run dev` shows one clearly dated real preview observation, not simulated live stock.

[Contributing](CONTRIBUTING.md) · [Security](SECURITY.md) · [MIT license](LICENSE)

Orchard is independent of Apple. Apple names and product data belong to their respective owners. Public community investigations in [apple-store-helper #126](https://github.com/hteen/apple-store-helper/issues/126), [#131](https://github.com/hteen/apple-store-helper/issues/131), and [Apple Pickup Watcher](https://github.com/ENCHIGO/apple-pickup-watcher) helped explain endpoint and rate-limit behavior. Orchard's implementation is independent; no code from those projects is included.
