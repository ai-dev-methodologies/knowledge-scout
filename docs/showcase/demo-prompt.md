# Demo prompt

Open the SSH tunnel first (`ssh -N -L 8790:127.0.0.1:8790 <ssh-host>`), put the key in `KS_KEY` (read from the key file, not typed), and connect Claude Code with [mcp.json.example](mcp.json.example). Then paste:

```
Use only the ks_search tool of the ks-remote server (the knowledge-card search).
Run exactly two searches and judge the candidates of each:
1. Keyword search: terms ["sqlite", "wal"], query "SQLite write-ahead log mode".
2. Natural-language search: terms [] (an empty list), query "컨테이너 헬스체크 프로브 재시작".
For each search, list the candidates with their card ids and judge them A (same problem),
B (borrowable technique) or C (same field only). Quote the evidence from the card's summary
or problems in one short phrase. Do not use cards that are not in the results.
Finish with one table: search | tier | id | title | evidence phrase.
```

Expected on the sample pool: the keyword search returns the SQLite card first (`05cae033cedcdc82`), the natural-language search returns the container health-check card first (`327d972d153b16e4`). More golden queries are in `examples/sample-data/queries.json`.
