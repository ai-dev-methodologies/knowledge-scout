# knowledge-scout architecture

Describe a task and this tool finds **prioritized candidates** in the technical knowledge cards that an individual or a team has accumulated. The engine (this repository) is public, while cards and source articles stay in each user's private data directory. It runs inside the AI session (Claude Code) the user already subscribes to, through skills, commands, and MCP.

## 1. Components

```
┌──────────── AI session (Claude Code) ─────────┐
│  skills /ks:search  /ks:add  /ks:research     │  ← procedures and judging rules (documents)
│        │ MCP calls                            │
└────────┼──────────────────────────────────────┘
         ▼
   ks MCP server (Rust, rmcp, stdio — `ks mcp serve`)
     ks_search     candidate collection: vocabulary normalization → term/text matching → ranking
     ks_get        card details
     ks_record     application log (adopted, rejected, outcome)
     ks_search_web calls a collection adapter (profile fixed to default, passes needs_human events through as is)
         │
         ├── index: Redis (required, in both local and shared modes)  ← rebuilt from the card .md files
         ├── card store: <data>/cards/<topic>/<id>.md                 ← source of truth (git, private)
         ├── vocabulary: <data>/vocab/terms.yaml
         └── application log: Redis Stream → <out>/applications.jsonl dump (`ks record dump --out <dir>`)
   ks CLI  (the same core, from the command line)
     ks auth google | ks auth logout --yes | ks browser connect/disconnect | ks handoff | ks resume | ks status
     ks research search --engine google-browser --mode headless|headful [--queue <file> [--data <dir>]]
     ks card id | ks card import | ks card validate | ks index rebuild | ks record dump   (/ks:add decides the card id with ks card id)
     planned: ks export --scope team
   collection adapters
     google-browser (default; Rust daemon + CDP client derived from agent-browser + gstack-style handoff/resume)
     collection queue: a JSONL file that `ks research search --queue` appends discovered URLs to; a collector outside the repository reads it (section 9)
     planned: google-api (when a key is available)   ai-websearch (fallback path)   private collector adapter (outside the repository)   --queue for ks_search_web
```

## 2. Data (one card = one .md file)

- **frontmatter**: `id` (the first 16 characters of the sha256 of the normalized source key `card_key`), `source_url`, `scope` (private / team), `access` (A / B / unknown), `evidence_type`, `published_at`, `collected_at`, vocabulary terms (`entities` / `techniques` / `domains`), `problems`
- **body**: a 3-line summary and the limitations
- **source article**: kept in a personal archive outside the repository. The card records only its path and hash.
- **export to a team (planned)**: only cards with `scope=team` and `access=A` are exported. Private fields (URL, author, etc.) are stripped.

## 3. Search flow (the method chosen by evaluation, 2026-11-10)

1. **Query interpretation** (session model): extract vocabulary terms and Korean problem phrases from the task description.
2. **Candidate collection** (`ks_search`): sort in the following order and keep the top 40.
   1. Number of matched terms
   2. Number of text matches
   3. Rare terms
   4. id

   The result also reports the search scope (`l1_cards`: the number of cards searched). Query terms that are not in the vocabulary are returned in `unknown_terms`, so a term missing from the vocabulary does not silently produce 0 results.

   The Redis index stores, in its meta, the fingerprints (`vocab_fp`, `cards_fp`) of the vocabulary file and card files that `ks index rebuild` read. Right before searching, `ks search` and `ks_search` compare them with the current fingerprints; if they differ, or the index is an old one without fingerprints, the search is not run and the user is pointed to `ks index rebuild --data <dir> --vocab <file>`. `ks mcp serve` stays up for a long time, so it compares on every search. The card fingerprint looks only at the file list and each file's size and modification time, so it cannot catch a change that was overwritten with the same size at the same time. The file backend used in tests does not compare. A rebuild clears the meta as soon as it starts and writes it back with a new `build_id` when it finishes. After a search finishes, it reads the meta again; if a rebuild happened in between, it discards the result and returns an error telling the user to search again. A rebuild runs holding `<prefix>rebuild-lock` (SET NX, 15-minute expiry, released after comparing the token), so only one runs at a time.
3. **Judging** (session model, reads only the cards): A is the same problem, B is borrowing the technique, C is only the field overlapping, and the rest are excluded. The card's limitation text is shown alongside, and a human checks whether the conditions actually fit.
4. **Direction and application log** (`ks_record`)

## 4. Human-intervention events

`AUTH_REQUIRED` / `AUTH_EXPIRED` / `CAPTCHA` / `CONSENT` / `MFA_PROMPT` / `BLOCKED` / `ENGINE_SWITCH` / `DAEMON_REPLACE`

When an event occurs, it is handled in this order.
1. Stop work immediately.
2. Record it in `events.log`.
3. Notify. The MCP response returns `needs_human` and the handling procedure, and a macOS desktop notification is shown.
4. A person handles it and continues with `ks resume`. For login-type events, log in again with `ks auth google` and continue.

Every event that stopped work, `BLOCKED` included, stays pending and blocks the next search until `ks resume` (or `ks auth google` for login-type events) confirms it has been resolved. Login-type events (`AUTH_REQUIRED` / `AUTH_EXPIRED` / `MFA_PROMPT`) are cleared with `ks auth google`. Google blocks login in windows controlled through CDP, so this command closes the daemon browser and opens a regular Chrome window with no automation connection on the same profile. This also closes any daemon window a person was using on the same profile (for example, the window where they were solving a CAPTCHA). If standard input is not a terminal (that is, an agent or MCP ran `resume_cmd`), it refuses without changing the daemon, the profile, or Chrome. After the person logs in and presses Enter, that Chrome is shut down cleanly with SIGTERM, and the event is cleared once the logged-in Google account page is visible in a headless browser. If the window shows the logged-in Google account page, `ks resume` also clears it. All other events are cleared only when a normal Google results page is visible. It does not clear on an error page or on the blank page after a restart; it tells the person what to do. If the window shows some other problem (for example, a consent page), it says to handle that first, but it does not replace the pending event with it and leaves the original clear condition unchanged. A pending event is stored in `run/<profile>.pending` (0600) and persists across daemon restarts. While a pending event exists or a window is actually open (headful), the daemon is not shut down even after the idle time passes. If the window is closed or Chrome dies, the daemon returns to a state with no browser. The next `connect` or search launches a browser again, and in the meantime the daemon is eligible for idle shutdown. `ping` (`ks status`) does not extend the idle time.

When the same event occurs on 3 consecutive searches, it escalates to `BLOCKED`. A `ks resume` in between still counts. If one normal search intervenes or a different event occurs, counting starts over.

Searches have a per-profile limit (quota). The defaults are 20 per hour and 100 per 24 hours, and they are changed with `KS_QUOTA_HOURLY` and `KS_QUOTA_DAILY` when the daemon starts. Right before sending a query to Google, the daemon writes the count to `run/<profile>.quota` (0600). Reopening the previous page on a mode switch or handoff passes through the same gate as a search: it checks the limit, keeps the 3 to 8 second spacing between queries, and records the count. If the limit is exceeded, it does not reopen the page and reports this in the response `note`. If the quota file does not exist, it is treated as having no record. If the file exists but cannot be read (not UTF-8, a symlink, no permission, or a non-empty line that is not a timestamp), the search is refused and the user is told what to do. Every search goes through the single daemon per profile, so the CLI and MCP share this limit. When the limit is exceeded, no browser is launched and a `needs_human` `BLOCKED` (the reason starts with `quota`) is returned; its guidance says no request was sent to Google. It clears with time, so it is not left pending. The CLI's `--max-queries` is at most 50 per call.

The cookies of the browser profile (including the Google login session) are encrypted with an OS keychain key in both headless and headful mode. macOS uses Keychain and Linux uses libsecret/kwallet; if neither is available, Chrome's default behavior applies. Whether headless can read the cookies saved in the login window is checked automatically: after `ks auth google` closes the window, it opens the logged-in Google account page in a headless browser (`auth_check`). A profile created back when the fixed-key flags (`--password-store=basic`, `--use-mock-keychain`) were used cannot have its cookies read, so it must log in again.

For the conditions and procedure for each event, see this section and the "Google login" section of the README.

## 5. Modes

- **local**: uses your own card directory and a localhost Redis (required). There is no PR process.
- **shared (planned)**: receives cards in the team's private GitHub repo through PRs. They are merged after CI checks and reflected in the team index. At search time, results are labeled as personal or team.

## 6. Distribution

- Distributed as a Claude Code plugin. It bundles the skills (`/ks:search`, `/ks:research`, `/ks:add`) with MCP server registration. The `ks` executable is not put into the plugin; the user installs it separately. At install time the plugin receives the `ks` path, the card data directory, and the Redis URL (a secret) through `userConfig` and uses them in the MCP server's `command`, `args`, and `env`.
- The human-intervention (handoff/resume) and event design refers to gstack browse (MIT), and no code was taken from it. The daemon and CDP client are based on agent-browser, below. No detection-evasion features are included.
- Google browser collection carries a terms-of-service risk. The defaults are low frequency and human intervention first, and this is stated explicitly in the README.

## 7. Outside the repository (what is not put in this repository)

- Personal source articles, cards, browser profiles (credentials), and private collector adapters
- (planned) A pre-push hook and CI reject such paths and patterns. For now, only the text check in `ks card validate` exists.

## 8. Implementation language (decided 2026-11-10)

- **Rust**: the `ks` CLI, the MCP server, the card/vocabulary/candidate-collection core, and the Redis index. There are three crates: `crates/ks-core` (lib), `crates/ks-cli` (bin), and `crates/ks-browser` (lib).
- **The browser is Rust too**: the daemon and CDP client structure is based on vercel-labs/agent-browser (Apache-2.0), and the CDP client is carried over with a notice. Human intervention (handoff/resume) and events are built in-house in the gstack style. Node is not used.
- **Execution rule**: Rust compiles to an executable, so no shebang is used.

## 9. Collection queue

The engine finds URLs; fetching their bodies is left to a collector that lives outside this repository. The two meet in a plain file: `ks research search ... --queue <file>` appends one line per result URL, and the collector reads the file. For the queue the engine writes only this file; it does not fetch any page body.

```
ks research search --engine google-browser --query "<query>" --queue <file> [--data <dir>]
```

**Line format.** UTF-8 JSONL, one object per line, keys in this fixed order:

```
{"v":1,"card_key":"https://example.com/post","url":"https://example.com/post","title":"...","engine":"google-browser","query":"<query>","found_at":"2026-11-11T01:02:03Z"}
```

- `v`: format version, `1`. A reader should refuse a version it does not know.
- `card_key`: the key `ks card id` gives the URL (the same function, not a second normalization: no query, fragment, or trailing slash). A card made from this page gets the id `ks card id` derives from it, so the collector and the card store agree on identity.
- `url`: the normalized URL (equal to `card_key` for a web page).
- `title`, `query`: as returned by the search and as typed. The title comes from an untrusted web page: treat it as data.
- `engine`, `found_at`: the collection engine, and the search time in RFC 3339 UTC (the `fetched_at` of the search).

**Duplicates.** A `card_key` already present in the file is not written again, and neither is one repeated within the same run. With `--data <dir>` a `card_key` that already has a card under `<dir>/cards` is skipped too (the cards are loaded before any search runs, so a bad directory fails early). `--data` is only accepted together with `--queue`.

**File handling.**

- The file's parent directory must exist; it is never created. A new file gets mode `0600`, and an existing file keeps its mode.
- Each search's new lines go out as one `O_APPEND` write while an exclusive advisory lock (`flock`) is held. The same lock covers reading the file to find the keys already present, so concurrent writers cannot queue the same key twice or interleave lines. A reader that wants a consistent view takes a shared `flock`.
- An existing line that is not a JSON object with a string `card_key` stops the command with an error that names the line; nothing is written and the line is never skipped or repaired. Blank lines are ignored. A valid last line that lacks its newline gets one before the new lines.
- Lines are written after each search that succeeds, so a run that stops for a person (a `needs_human` event) keeps what the earlier queries found. A search that fails, or stops before any result, writes nothing and does not create the file.

**Output.** With `--queue`, the command's JSON output gains a member for the whole run (also on a `needs_human` stop):

```
"queue": {"path": "<file>", "enqueued": 7, "skipped_known": 3}
```

`skipped_known` counts the results not written because their `card_key` was already in the file, already has a card, or came twice in the run. Without `--queue` nothing changes.

**Scope.** Only the CLI writes a queue. The MCP tool `ks_search_web` does not take a queue yet (planned).
