# Agent clients

You can use Orchard’s desktop app without any of these steps. This guide is for terminal users and agent clients.

## Install the command-line tool

**Apple Silicon Mac:** download [`orchard-macos-arm64.tar.gz`](https://github.com/ClancyDennis/orchard/releases/download/v0.2.1/orchard-macos-arm64.tar.gz), open it to unpack, and locate the `orchard` executable inside the `orchard-macos-arm64` folder. You can run it using its full path; for example:

```sh
"$HOME/Downloads/orchard-macos-arm64/orchard" --help
```

If you unpacked it elsewhere, use that path instead. The binary is unsigned; the [README’s first-launch note](../README.md#install-on-your-mac) also applies.

**Build from source** on macOS, Linux, or Windows with Rust stable and a C build toolchain installed:

```sh
git clone https://github.com/ClancyDennis/orchard.git
cd orchard
```

Then install:

```sh
cargo install --path crates/orchard-cli --locked
orchard --help
```

A source install puts the binary in `~/.cargo/bin/` (`orchard.exe` on Windows). If you use the prebuilt archive, it stays wherever you unpacked it. Use its absolute path when configuring a desktop client whose PATH differs from your shell.

## Codex

```sh
codex mcp add orchard -- /absolute/path/to/orchard mcp
```

Equivalent `config.toml` entry:

```toml
[mcp_servers.orchard]
command = "/absolute/path/to/orchard"
args = ["mcp"]
tool_timeout_sec = 300
```

See [Codex MCP configuration](https://developers.openai.com/codex/mcp/).

## Claude Code

```sh
claude mcp add --transport stdio orchard -- /absolute/path/to/orchard mcp
```

For a client accepting an MCP JSON configuration:

```json
{
  "mcpServers": {
    "orchard": {
      "command": "/absolute/path/to/orchard",
      "args": ["mcp"]
    }
  }
}
```

See [Claude Code MCP configuration](https://code.claude.com/docs/en/mcp). Registration command syntax was verified with the installed clients' help. Orchard requires no API key or LLM service.

## Tools

| Tool | Purpose |
|---|---|
| `catalog_list` | List device IDs; pass `device_id` to get exact SKUs, dimensions, and Apple purchase links. |
| `catalog_refresh` | Read a category from Apple buy pages, or add a new family page via `apple_url`. |
| `inventory_check` | Perform one paced check; save its real result and return `retry_at_ms`. |
| `inventory_latest` | Read the latest saved observation without a network request. |
| `inventory_history` | Read a date range, including failed observations; results are bounded and newest first. |
| `inventory_status` | Read settings, cooldowns, the next check, and connection metadata. |

Example prompt:

> Use Orchard to list Mac mini configurations, then check the standard model I choose near ZIP 10001. Report the observation time, available stores, and Apple's pickup quote. If deferred, wait until retry_at_ms before checking again. Do not change my desktop tracker.

Each tool returns structured JSON and a matching text block. `state: checked` means a request succeeded; read `available_count` for stock. `failed`, `deferred`, `unknown`, and `pending` do not mean unavailable. `last_known` is historical and has its own timestamp. The API does not reserve anything or purchase products.

## CLI polling

```sh
orchard catalog --category iphone
orchard catalog --device iphone-duo
orchard check --device iphone-duo --zip 10001 --part MK1G4LL/A
orchard latest --device iphone-duo --zip 10001 --part MK1G4LL/A
orchard watch --device iphone-duo --zip 10001 --part MK1G4LL/A --interval 120 --timeout 1800
```

`watch` emits NDJSON, stops when stock appears or the timeout expires, and sleeps until the returned retry time. `--full` on `check` includes every inventory row. Exit codes: 0 = completed query/read (not necessarily in stock), 1 = failure, 2 = deferred check or watch timeout, 3 = invalid command/options.

`check` and `watch` override the target only for their own observation; they do not switch the desktop's selected device or saved schedule. All clients using the same database share an atomic request gate and an OS file lock. Long browser checks cannot overlap another client's request. Do not poll continuously through retries or alternate transports to bypass a cooldown.

## Storage

By default, the CLI and desktop use the same `local.orchard.iphone-tracker/inventory.sqlite3` application-data directory. `orchard status` shows the path. Use `--database /absolute/path/inventory.sqlite3` for an explicitly separate workspace or test fixture. A separate database does not give your public IP additional Apple request quota.

The MCP server is stdio only. It has no HTTP listener, login, remote account, or background daemon. The client owns its lifetime. Use the desktop app or a bounded `orchard watch` process for ongoing collection.
