# Changelog

All notable changes are recorded here. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and versions follow [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
- `ks --version`.
- A separate Redis instance for development, samples and tests (`docker-compose.test.yml`, `docs/servers.md`), a test
  guard that refuses the personal instance, and `scripts/ks-redis-test.sh`, which serializes Redis tests.
- CI runs on `rc/**` and `release/**` pushes and can be started by hand; release candidates also produce a static Linux
  x86_64 (musl) `ks` binary as a CI artifact.
- Remote MCP: `ks mcp serve --http <addr:port>` serves stateless Streamable HTTP at `/mcp` with only `ks_search`
  (keywords or a natural-language question) and `ks_get`; web search and application records stay on stdio.
  Requests need `Authorization: Bearer <key>`; readers get public cards only and `ks_get` without source fields, admins
  and owners the full card. Host and Origin are checked. It refuses to start on a non-loopback address without an
  active key, with `--no-auth`, or for readers when a card is not `scope: public`.
- `ks key issue|list|revoke`: operator-issued API keys (owner, admin, reader). A key is shown once; Redis keeps only
  its SHA-256 and the metadata (purpose, user, role, issue time).
- Card scope `public` (`ks card import --scope public`).

## [0.1.0]

First version: card storage and validation, candidate search over files and a Redis index, the MCP server, and the
Google browser engine.

[Unreleased]: https://github.com/ai-dev-methodologies/knowledge-scout/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/ai-dev-methodologies/knowledge-scout/releases/tag/v0.1.0
