# Search, RAG, and when to move on

[README](../README.md) | English | [한국어](ko/SEARCH-AND-RAG.md)

## Short answer

RAG (retrieval-augmented generation) means: search first, then let an AI read what the search found. In that broad sense knowledge-scout is RAG. It differs from the common recipe in two ways:

- **What it searches.** Short knowledge cards (title, problems, limits, evidence type, tags), not chunks of the raw articles.
- **How it searches.** Controlled-vocabulary tags and exact text matches (keyword search). There are no embeddings and no vector database. The AI session reads the candidates and judges them, so the server only counts matches and needs no model.

So the real question is not "RAG or not" but "what finds the candidates". The rest of this page says when this choice works, where it fails, and what to switch to.

## Terms in plain words

| Term | Meaning | Good at | Bad at |
| --- | --- | --- | --- |
| Keyword search (BM25) | Finds documents that share words with the query; rare shared words count more | Product names, exact terms | The same idea written in different words |
| Controlled vocabulary (tags) | Tags come only from a fixed list of terms | Collects different spellings under one tag | Someone must maintain the list; a question phrased differently from the list is missed |
| Embeddings (semantic search) | Turns a sentence into numbers so that sentences with similar meaning are close | Rephrasing; crossing languages (Korean question, English article) | Rare names and exact matches; needs a model; a new model means recomputing everything; hard to explain why something matched |
| Vector database | A store that finds the nearest number-lists quickly at large scale | Hundreds of thousands to millions of items | Not needed at small scale; Redis has this feature too |
| Hybrid search | Merges keyword and semantic results | Covers each side's weakness | More moving parts |
| Re-ranking | A stronger model re-orders the candidates | Precision | Cost and time. Here the session model's A/B/C judging plays this role |

## Conditions where this approach works well

- A few hundred to a few thousand cards, kept by one person or a small team. Rebuilding the index is fast and everything to fix is a file.
- Queries contain technical names, which is what tags and exact matches do best.
- You want to see why a card matched. The tool returns the matched terms and text, and the same input gives the same result.
- You do not want a model, an API key, or a GPU on the server.
- The judging is done by the AI session you already use.

## Limits, with the conditions of the measurement

- **Different wording is missed.** In our own evaluation, 47 of 59 expected cards (79.7%) were among the 40 candidates. All 12 misses had no matched term and no matched text at all. For queries that ask to borrow a technique for a different purpose, 17 of 24 (70.8%) were found.
- **Conditions of that number.** 50 queries, 59 expected cards, a private evaluation set, one judge, one run, so the margin is large (roughly 9 percentage points). The search terms were chosen with the vocabulary list in view. The real `/ks:search` skill does not show the list, so real use is likely lower; it has not been measured. In one logged real use, 9 of 12 query terms were not in the vocabulary.
- **Not on a card, not found.** Details dropped when an article was shortened to a card cannot be found by any card search. Articles that were never turned into cards cannot be found at all.
- **The vocabulary needs care.** New cards bring new terms, and a changed vocabulary means rebuilding the index and restarting the MCP server.
- **Ranking counts matched terms.** Three common terms can outrank one rare, decisive term. With few matches per query this is invisible; with many more cards it may matter. Not measured.
- **A search takes effort.** One measured run took 58 seconds and about $0.60 of model usage. It suits a few searches a day by a person.

## Signs that it is time to move to RAG or a vector database

The thresholds are proposals, not measured conclusions. Fix them before you measure.

| What you see | Move to |
| --- | --- |
| On a fresh set of real queries, fewer than 85% of expected cards are in the 40 candidates, even after merging the query sentence into the score | Embed each card's short text (title, problems, summary) and store it in the same Redis |
| Rephrased queries score more than 5 points below the originals; or most query terms are not in the vocabulary even after the model sees the list | Embeddings |
| You often describe a situation without naming a technology, or want a technique from another field | Embeddings; if still not enough, search raw text chunks |
| You need to search articles that have no card, or a detail that is not on any card | Embeddings of raw text chunks (RAG in the narrow sense) |
| Korean questions must find English articles directly | Multilingual embeddings. Text matching cannot cross languages, and the Redis search engine's stemming list has no Korean |
| A search often fills all 40 candidate slots | Rank by score (BM25 or embedding distance) instead of by number of matched terms |
| About 20,000 cards, or hundreds of articles arriving per week | Re-check ranking and index freshness first |
| 1 million vectors or more, or tens of queries per second | An HNSW index in Redis; only then a dedicated vector database |

In one line: "different words" confirmed by measurement points to embeddings, "not on the card" points to raw-text search, and "over a million items" points to a vector database. Also keep a log of real searches; without one none of these signs can be seen.

## The path inside the same Redis

Redis already has the pieces. Its documentation recommends a FLAT vector index under 1 million vectors and lists `FT.HYBRID` (keyword plus vector in one query, since Redis 8.4.0). The order we would follow, only if the measurements above call for it:

1. Widen the candidate gate without embeddings: let the model see the vocabulary and also score the query sentence.
2. Embed card short text and merge the vector result with the keyword result. This adds an embedding model or API call to the server, which today needs none.
3. Search raw text chunks only if the "not on a card" or "language" signs appear.

**Backlog notice: `FT.HYBRID` (keyword plus vector hybrid search) is on the backlog and will be provided later.** knowledge-scout does not use it, or any vector search, today.

## External references

- BEIR benchmark (Thakur et al., 2021): BM25 stays a strong baseline outside the training domain. https://arxiv.org/abs/2104.08663
- Anthropic, Contextual Retrieval (2024-09-19): merging embeddings with BM25 and re-ranking lowered top-20 retrieval failures from 5.7% to 1.9% in the vendor's own test; for a knowledge base under about 200,000 tokens it suggests putting everything in the prompt. https://www.anthropic.com/news/contextual-retrieval
- Li et al. (2024), RAG or long-context LLMs: long context scores higher on average, RAG costs less. https://arxiv.org/abs/2407.16833
- LaRA (2025): the better choice depends on model, context length, and task; there is no universal answer. https://arxiv.org/abs/2502.09977
- Redis, vectors: FLAT index for small data sets (under 1 million vectors), HNSW for larger. https://redis.io/docs/latest/develop/ai/search-and-query/vectors/
- Redis, `FT.HYBRID`: full-text and vector search in one command, since 8.4.0. https://redis.io/docs/latest/commands/ft.hybrid/
- Redis, stemming: the list of supported languages (no Korean when checked on 2026-11-11). https://redis.io/docs/latest/develop/ai/search-and-query/advanced-concepts/stemming/

These are mostly vendor tests or small studies; read them as hints, not as proof for your data. The best evidence is a measurement on your own queries.
