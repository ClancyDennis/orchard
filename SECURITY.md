# Security

Please use GitHub's private vulnerability reporting for security-sensitive issues, or email clancyd@gmail.com.

Orchard's MCP server uses standard input/output and opens no listening HTTP port. The optional browser transport uses a loopback-only debugging connection and its own temporary profile. Catalog URLs are limited to Apple US buy pages. Inventory data, schedules, and connection diagnostics stay in the local application-data directory unless the user exports them.

Only install MCP servers you trust: the local client can request public inventory checks and read the Orchard database configured for that server. Giving a client a different `--database` isolates its history but also creates a separate pacing gate; do not use that to evade shared-IP limits.
