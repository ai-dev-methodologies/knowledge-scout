# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Verification evidence for a release (commit, CI run, review verdict) goes in the GitHub Release notes, not in this file.

## [0.2.0] - unreleased

Scope for v0.2.0. Only the items under "Added" exist in the code today.

### Added
- Collection queue: `ks research search --queue <file> [--data <dir>]` appends the URLs of discovered results to a JSONL file for a collector outside this repository to read.
- README rewritten for first-time readers (what it is, why, where it fits and where it does not, is it RAG). Installation and operations details moved to `docs/`.

### Planned for v0.2.0 (defined, not implemented yet)
- Remote MCP over HTTP that serves search only (`ks_search`, and `ks_get` with fields hidden by role), with operator-issued API keys stored hashed in Redis. See [docs/REMOTE-MCP.md](docs/REMOTE-MCP.md).
- A small public sample card pool to try the tool without your own cards.
- Release CI and a separate test Redis.

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
