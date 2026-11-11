# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow [Semantic Versioning](https://semver.org/). Verification evidence for a release (commit, CI run, review verdict) goes in the GitHub Release notes, not in this file.

## [0.2.0] - 2026-11-12

### Added
- `ks --version`.
- Collection queue: `ks research search --queue <file> [--data <dir>]` appends the URLs of discovered results to a JSONL file for a collector outside this repository to read.
- Remote MCP: `ks mcp serve --http <addr:port>` serves stateless Streamable HTTP at `/mcp` with only `ks_search` (keywords or a natural-language question) and `ks_get`; web search and application records stay on stdio. Requests need `Authorization: Bearer <key>`; readers get public cards only and `ks_get` without source fields, admins and owners the full card. Host and Origin are checked. It refuses to start on a non-loopback address without an active key, with `--no-auth`, or for readers when a card is not `scope: public`. See [docs/REMOTE-MCP.md](docs/REMOTE-MCP.md).
- `ks key issue|list|revoke`: operator-issued API keys (owner, admin, reader). A key is shown once; Redis keeps only its SHA-256 and the metadata (purpose, user, role, issue time).
- Card scope `public` (`ks card import --scope public`).
- A small public sample card pool (`examples/sample-data`) with golden queries, to try the tool without your own cards.
- Showcase kit (`deploy/showcase`, [docs/SHOWCASE.md](docs/SHOWCASE.md)): Docker Compose files for Redis and the remote MCP server on 127.0.0.1 with the sample pool, an install runbook that issues owner and reader keys into 0600 files, a demo prompt and an `.mcp.json` example for Claude Code.
- A separate Redis instance for development, samples and tests (`docker-compose.test.yml`, `docs/servers.md`), a test guard that refuses the personal instance, and `scripts/ks-redis-test.sh`, which serializes Redis tests.
- CI runs on `rc/**` and `release/**` pushes and can be started by hand; release candidates also produce a static Linux x86_64 (musl) `ks` binary as a CI artifact.

### Changed
- README rewritten for first-time readers (what it is, why, where it fits and where it does not, is it RAG). Installation and operations details moved to `docs/`.

## [0.1.0]

### Implemented
- Card model, validation, vocabulary normalization, candidate collection and ranking (`ks card import`, `ks card validate`, `ks search`)
- Redis index rebuild (`ks index rebuild`)
- Application log (`ks record`, Redis Stream) and JSONL dump (`ks record dump`)
- MCP server over stdio (`ks_search`, `ks_get`, `ks_record`, `ks_search_web`)
- Google browser collection daemon: login (`ks auth google`), search (`ks research search --engine google-browser`), human-intervention events and `handoff` / `resume`, search quota and query spacing, macOS desktop notifications
- Claude Code plugin `ks` (`plugins/ks`) and marketplace manifest (`.claude-plugin/marketplace.json`): the skills `/ks:search`, `/ks:research`, `/ks:add`, plus MCP server registration. The `ks` executable is not bundled in the plugin; users install it separately, and the plugin uses the path, data directory, and Redis URL it received at install time.
- CI: full tests on ubuntu with the Redis and Chrome gates enabled (`.github/workflows/ci.yml`)

### Backlog (not implemented, no date)
- `ks export --scope team`
- Team sharing mode: PR and CI checks on the team repository, personal/team provenance labels
- `google-api` and `ai-websearch` collection adapters, and a private collector adapter kept outside the repository
- `--queue` for the MCP tool `ks_search_web` (only the CLI writes a queue for now)
- Per-event procedure documents for human-intervention events, and a pre-push hook
- `FT.HYBRID` (keyword + vector hybrid search in one Redis query): backlog, to be provided later. See [docs/SEARCH-AND-RAG.md](docs/SEARCH-AND-RAG.md).

[0.2.0]: https://github.com/ai-dev-methodologies/knowledge-scout/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/ai-dev-methodologies/knowledge-scout/releases/tag/v0.1.0
