# How Orchard works

## Apple connections and limits

Automatic mode first uses `/shop/retail/pickup-message`. If that endpoint returns 404 or 410, it can use `/shop/fulfillment-messages` through a normal browser session. Direct-only and browser-session modes are also selectable. A browser session requires installed Chrome, Edge, or Chromium; it uses its own temporary profile and no personal Safari/Chrome credentials.

**A 541, 429, or an active Retry-After does not trigger endpoint switching.** All methods share a persisted minimum of one minute between checks. Your slower configured rate is preserved. The first 541/429 starts a fifteen-minute cooldown, increasing to thirty and sixty minutes after consecutive failures. Manual clicks, restarts, and device changes cannot shorten a cooldown. Two minutes is a reasonable starting rate; no rate guarantees Apple will accept requests, and other clients on the same public IP may share its limits.

Diagnostics record the endpoint, HTTP status, transport, timing, and optional browser cookie **metadata**. Cookie values, authentication headers, raw response bodies, and personal browser data are excluded. Logs stay local and rotate. A successful HTTP response must still contain all requested configurations for each returned store before Orchard accepts it as inventory.

Orchard uses Apple-native paths only. The old third-party collector has been removed. Legacy tracker addresses are decoded solely during migration into local `apple://us/...` identities; archived observations retain their original source label. These identities are database keys, not network URLs.

## Catalog scope

The catalog parser reads `productSelectionData` as JSON; it does not execute Apple's page scripts. iPhone/iPad pages expose sellable SKUs directly. Mac pages include standard SKU configurations and customizable entries; entries without a sellable pickup SKU are excluded. Apple product redirects supply memory/storage descriptions where available. Incomplete descriptions are labeled with the SKU and a detail warning. Custom-built Macs and every possible CTO combination are not supported.

This release is **US-only**. New product-page layouts may require a parser update. Catalog freshness and inventory freshness are separate: a newly refreshed catalog does not establish stock.

## Local data

The desktop, CLI, and MCP server use the same Rust engine and database format. Data stays under `local.orchard.iphone-tracker` in the platform’s application-data directory. Desktop **Settings** and `orchard status` show the exact path.

Each device/configuration/carrier/ZIP selection has a separate history. The desktop schedules one selected target at a time; CLI/MCP clients can check other targets without changing that selection. They share pacing and cooldowns when using the same database. An operating-system file lock also prevents overlapping checks from multiple Orchard processes.

`--database /absolute/path/inventory.sqlite3` selects a separate workspace. This does not grant extra Apple request quota. Existing migrations preserve snapshot IDs, timestamps, failures, and schedules.

## Graphs and patterns

The graph counts available **store/configuration combinations**, not units in a store. One configuration available at three stores produces a count of three. Selecting all configurations can produce a larger count. Failed checks and missed collection periods remain gaps.

A fresh installation has no historical inventory. The first successful check adds one point; the timeline grows while collection runs. Orchard does not invent earlier observations.

**When to watch** uses local statistical analysis, including time-weighted availability and the number of observed days. It excludes failures and unreliable gaps. These are historical hints, not Apple’s update schedule. Sparse history may produce no recommendation.

## Development

See [Contributing](../CONTRIBUTING.md) for build prerequisites and tests. The repository contains:

- `crates/orchard-core`: catalog, transports, SQLite, pacing, history, and analysis.
- `crates/orchard-cli`: JSON CLI and stdio MCP server.
- `src-tauri`: desktop integration and notifications.
- `src`: React UI.

Tests use fixtures and local HTTP servers. The official MCP client SDK test verifies handshake, tools, historical/unknown states, cooldown sharing, and bounded polling without Apple requests. A live inventory test is explicitly ignored by default.

`npm run dev` opens a read-only browser preview using one dated public inventory observation. Screenshot examples come from that preview; they do not represent live stock.

## Credits

Orchard is independent of Apple. Apple names and product data belong to their respective owners. Public community investigations in [apple-store-helper #126](https://github.com/hteen/apple-store-helper/issues/126), [#131](https://github.com/hteen/apple-store-helper/issues/131), and [Apple Pickup Watcher](https://github.com/ENCHIGO/apple-pickup-watcher) helped explain endpoint and rate-limit behavior. Orchard’s implementation is independent; no code from those projects is included.
