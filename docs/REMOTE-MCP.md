# Remote MCP (v0.2.0)

[README](../README.md) | English

**Status: defined for v0.2.0, not implemented in the code yet.** This page follows the v0.2.0 plan. Command names and flags can still change before the release; check `ks --help` once it ships. Today the MCP server only runs over stdio on your own machine (see [INSTALL](INSTALL.md)).

## What it is

A small HTTP mode of the same `ks` engine, so that a person who is not on the machine holding the cards can search them from their own Claude Code with a key you issued.

- **Search only.** The remote server registers exactly two tools: `ks_search` and `ks_get`. Web search, recording, collection, card creation, and index rebuilds stay with the operator on the server. `ks_search_web` and `ks_record` are not available over HTTP, even if a client calls them directly.
- **Keys, not accounts.** The operator issues API keys. The server keeps only a SHA-256 hash of each key in the instance's own Redis, with its role, purpose, and user label. The raw key is shown once, in a file only you can read.
- **Roles decide what a caller sees.**

  | Role | `ks_search` and `ks_get` |
  | --- | --- |
  | `reader` | Only cards with `scope: public`. `ks_get` returns a fixed list of fields: `id`, `title`, `summary`, `problems`, `limits`, `evidence_type`, `entities`, `techniques`, `domains`, `topic`, `published_at`. No source URL, source path, or card key. |
  | `admin`, `owner` | The full card Markdown, including source fields. |

- **Closed by default.** The server refuses to start on a non-loopback address unless at least one valid key exists, and refuses `--no-auth` on any non-loopback address. If the key store cannot be reached, requests are rejected (HTTP 503) and no tool runs. Requests carry a Host and Origin check.

## Operator: issue a key and start the server

```bash
# issue a key; the raw key goes only into the file (mode 0600, refuses to overwrite), stdout shows its id
ks key create --role reader --label <purpose> --subject <who> --out <file>
ks key list              # ids, roles, labels; never the key or its hash
ks key revoke <id>       # takes effect on the next request

ks mcp serve --data <data> --http 127.0.0.1:8790 --min-role reader
```

- `--min-role reader|admin|owner` sets the lowest role the server accepts. A pool that holds any card whose `scope` is not `public` refuses to start with `--min-role reader`.
- `--no-auth` is only for a loopback address and gives every caller the `reader` role.
- Host check: loopback names (`localhost`, `127.0.0.1`, `::1`) are always accepted, so an SSH tunnel works without extra settings. To serve other names, list them with `--allowed-host <name>[,<name>...]` (repeatable; omit the port). An empty value is ignored.
- Requests with an `Origin` header are rejected unless listed with `--allowed-origin`. Command-line clients send none.
- Each request writes one log line to stderr: key id, role, tool, latency, result code. The key itself is never logged.

## User: connect with the key

Open a tunnel to the operator's host, then point Claude Code at the local end. Replace the placeholders.

```bash
ssh -L 8790:127.0.0.1:8790 <ssh-host>
```

```json
{
  "mcpServers": {
    "ks-remote": {
      "type": "http",
      "url": "http://127.0.0.1:8790/mcp",
      "headers": { "Authorization": "Bearer ${KS_KEY}" }
    }
  }
}
```

Put the key in the `KS_KEY` environment variable (read it from the key file the operator gave you; do not paste it into a command line, where it would stay in shell history). Check that your Claude Code version expands `${VAR}` inside `headers`. If your settings restrict MCP servers with `allowedMcpServers`, allow this server too, or it is dropped without a warning.

Responses: 401 for a missing, wrong, or revoked key; 403 for a role below `--min-role` or a Host that is not allowed.

## Not covered

TLS and a public internet endpoint are not part of v0.2.0. The default path is the SSH tunnel above. Exposing the server directly is a separate decision for the operator.
