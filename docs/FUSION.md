# Fusion ranker (demo level)

[README](../README.md) | English

`ks fusion` is a second ranker beside `ks search`: the search lab's r4-unified candidate ported to Rust. It runs on Redis
and the CPU. A local bge-m3 embedding is optional, and a switch decides per query whether it is used. It is not wired into
`ks search` or `ks mcp serve` yet.

## What it does

1. **Index** (`ks fusion build`): the cards `ks search` would search become one Redis hash each, under the fusion key
   prefix (default `ksf:`). "The cards `ks search` would search" means: of cards sharing an id the last one wins, and
   cards with a `skip_reason` are left out. Each hash holds eight text fields of token codes (title, Korean title, keywords, summary, problems,
   limits, long summary, aliases), the card's vocabulary terms, and one binary payload for the ranking step. The card's
   1,024-number vector is stored too if you have one (FLOAT16, not indexed). One token table stores the document frequency
   and an association row for every token. The RediSearch index is created after the hashes exist, and the build waits for
   it to settle.
2. **Search**: one `HMGET` on the token table, then one weighted `FT.SEARCH` for the top 400 candidates and their
   payloads. A feature row is computed for each candidate, two linear models rank them (the first model's order picks the
   feedback cards for the second), and the top 40 are returned.
3. **Embedding switch** (`--switch auto|on|off`):
   - `off`: never calls the model.
   - `on`: always uses the embedding; a failed call is an error.
   - `auto` (default): embeds the query on a local llama-server while Redis answers. If the server fails or takes longer
     than 700 ms, that query is ranked lexically with the candidates already fetched, and a circuit breaker opens. While it
     is open there are no model calls. After 5 s one background probe checks the server again.
   - With fewer than 80% of the cards (or of a query's candidates) carrying a vector, `auto` ranks lexically.
   - Every result says which path it took (`path`, `reason`).

Text analysis: Hangul character bigrams, folded English words, alias linking through the vocabulary and an alias
dictionary. No morphological analyser.

## Commands

```
ks fusion build --data <dir> --aliases <aliases.json> --model <fusion-model.json> \
    [--vocab <terms.json>] [--long-dir <dir>] [--vectors <file> | --embed-url http://127.0.0.1:<port>] \
    [--enrich <sidecar.jsonl> --enrich-weight <n>] [--prefix ksf:] [--digest-out <file>]
ks fusion search --query "<text>" [--term <t> ...] [--switch auto|on|off] [--embed-url http://127.0.0.1:<port>] [--limit 40] [--prefix ksf:]
ks fusion batch [same engine options] < queries.jsonl > rankings.jsonl      # {"qid","query","terms"?} per line
```

- **The model file** (`fusion-model-r4.json`) holds the linear weights, the feature order and every analyzer and switch
  constant. It is written by the search lab (`node algos/r4-unified-export.mjs model`), not by this repository. Loading
  checks the file against this engine and refuses a mismatch: feature order, analyzer constants, token probes, and any
  column the engine does not compute. The build stores the model, vocabulary and alias dictionary in `<prefix>m`, so a
  search needs only Redis.
- **`--vectors`**: records of 16 ASCII id bytes plus 1,024 little-endian FLOAT16 values. `--embed-url` computes them
  instead, 32 cards per request.
- **`--enrich`**: a sidecar keyed by card id (`questions_ko`, `title_en`, `summary_en`). It adds one more text field to
  the first stage, with an integer weight. The ranking models do not read it. It is off by default.
- **Embedding server**: only a loopback URL is accepted. The client posts to `/v1/embeddings` (llama.cpp's OpenAI-style
  endpoint).
- **Input limits**: the same as `ks search` (512 characters per query, 32 terms of 64 characters).

## Demo

```
export KS_REDIS_URL=...           # the Redis ks uses (or KS_REDIS_HOST / KS_REDIS_PORT / KS_REDIS_PASSWORD)
cargo build --release
LAB=<search lab dir>              # corpus/, enrich/aliases-v1.json, runs/ks-fusion-rust/ (model + vectors from the lab's export)
bash scripts/fusion-demo.sh $LAB/corpus $LAB/enrich/aliases-v1.json $LAB/runs/ks-fusion-rust/fusion-model-r4.json \
    $LAB/runs/ks-fusion-rust/card-vectors-f16.bin $LAB/corpus/knowledge
# with a local CPU bge-m3 server, the second run of each query takes the embedding path:
KS_EMBED_URL=http://127.0.0.1:18092 KS_FUSION_SKIP_BUILD=1 bash scripts/fusion-demo.sh <same arguments>
```

The script builds the index under `ksf:`, then runs five searches twice each: once with `--switch off` and once with
`--switch auto`. Without a server, `auto` reports `path off (embed-error)` and returns exactly the `off` ranking. Each
demo search is a separate process; in a long-running `ks fusion batch` the breaker then opens and later queries report
`breaker-open` without calling the server.

## Safety rules

- **Key prefix.** Only the fusion namespace is accepted: `ksf:` or `ksf-<name>:`.
  - `ks:` and `ks-eval-<name>:` belong to plain `ks` (its index, cards, application stream and API keys) and are refused.
  - So is any prefix that equals or overlaps `KS_KEY_PREFIX`.
- **What a build may replace.** A build writes only to an empty prefix or over a fusion index (`<prefix>m` says
  `schema: ks-fusion-index`), and every key there must have a shape the build writes itself: `<prefix>m`, `<prefix>t`,
  `<prefix>c:<n>`.
  - It refuses, deleting nothing, and names the keys, when any other key is under the prefix. That includes the keys of
    plain `ks` (`<prefix>meta`, `<prefix>apply`, `<prefix>rebuild-lock`, `<prefix>card:*`, `<prefix>auth:*`) and also any
    unknown key, even next to a fusion meta.
  - It also refuses an index without fusion meta.
  - The check runs inside the purge itself, and the purge deletes only keys of those three shapes.
  - These checks run before any card is read or embedded.
  - The cards, model, vocabulary, alias dictionary, vectors and sidecar are all read and checked before the old index is
    deleted.
- **A failed build.** A build that fails while writing leaves an incomplete index (`complete: 0`). Searches refuse it;
  run the build again.
- **Stale index.** Every build gets a new `build_id`.
  - A search reads the meta again when it ends, and drops its result with an error when the index was rebuilt or is
    incomplete.
  - With `--data <dir>`, `search` and `batch` refuse an index built from other card files than the ones under
    `<dir>/cards` now (the same fingerprint as `ks search`).
- **Vectors.**
  - A query vector must have the model's dimension (1,024) with finite values. Otherwise switch `auto` ranks that query
    lexically (reason `dim-mismatch` or `embed-error`) and `on` fails.
  - A card vector of the wrong size, or with an infinite or NaN value, is not stored or used: the card counts as a card
    without a vector, including for the 80% rule. `--vectors` reports how many it refused.
- **Batch.** A failing query, or bad input on one line, becomes that line's `{"error": ...}` and the run goes on.
  - When the index was rebuilt (complete, new build id), `batch` reopens it once and answers the line from the new index.
  - While it is incomplete, each line gets an error.
- **Scope.** Every hit carries the card's `scope`. The CLI returns every card, as `ks search` does. A reader-facing
  integration (`ks mcp serve --http`) will have to keep only `scope: public`, as its `ks_search` does.
- **One deliberate difference from the JS reference.** When every association score of a query is quantized to 0, the
  JS divides 0 by 0, and its NaN weights drop the query's own tokens from the first stage. This engine gives those
  weights 0 and keeps the query's own clauses. It does not happen on the lab's dev queries: parity there is exact.

## Measured (2026-11-15, 5,607 cards, the lab's dev queries)

- **Same ranking as the JS reference.** The built index is byte-identical to the JS one: every payload, every text field,
  the 74,554-row token table and the average field lengths. On 112 dev forms in each mode, both paths give the same top 40
  with the same scores, bit for bit. (The lab corpus has no duplicate ids and no cards with a `skip_reason`.)
- **Quality in the lab harness**, held-out P@10 out-of-fold (query / terms):
  - without the embedding: 0.744 / 0.794;
  - with the embedding (cached query vectors): 0.802 / 0.808.
  - These equal the JS module. The search lab's own notes have the context; the lab's
    0.796 for query mode embeds every query live.
- **Engine time without the embedding**: p50 15 ms, p95 19 ms per query. Most of it is `FT.SEARCH`.
- **With a live CPU embedding** (bge-m3 FP16, llama-server -t 4, through the lab harness): p50 about 190 ms per query, and
  held-out P@10 0.796 / 0.808. Both equal the JS module run live.
- **Fallback**: with no embedding server, `auto` returns the `off` ranking for 112 of 112 forms in each mode.

## Limits

- The models were fitted on the lab's dev queries, so the shipped numbers on those queries are fitted numbers.
- A rebuild is not locked against a concurrent rebuild of the same prefix.
- Without `--data`, a search checks only that the index is complete and unchanged, not that the card files are.
- There is no Korean version of this page yet.
