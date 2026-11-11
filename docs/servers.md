# Redis servers

Two Redis instances run side by side on a development machine. They use the same image and the same search settings
(`docker-compose.yml`), but never share a port, a container name, a data directory, or a password.

| Name | Purpose | Host port | Container | Compose project | Data | Resources | Env file |
|---|---|---|---|---|---|---|---|
| personal | the real card index | 6390 | `ks-redis` | `knowledge-scout` (default) | `./.ks-data/redis` (AOF) | no limit | `./.env` |
| dev / sample / test | development, sample data, automated tests | 6391 | `ks-test-redis` | `ks-test` | `~/.ks/test/data` (no persistence) | 256 MB container cap, `maxmemory` 128 MB, 0.5 CPU | `~/.ks/test/redis.env` (0600) |

Both bind `127.0.0.1` only.

## Personal instance

Unchanged: `docker compose up -d --wait` in the repository root. `KS_REDIS_CONTAINER` and `KS_REDIS_DATA` are optional
variables with the old values as defaults, so the resolved configuration is identical to before.

## Test instance

```sh
# once: write ~/.ks/test/redis.env (mode 0600) with KS_REDIS_PASSWORD (a fresh secret), KS_REDIS_PORT=6391,
# KS_REDIS_CONTAINER=ks-test-redis, KS_REDIS_DATA=$HOME/.ks/test/data
docker compose -f docker-compose.yml -f docker-compose.test.yml -p ks-test --env-file ~/.ks/test/redis.env up -d --wait
# once per fresh server: the marker that the test guard requires
docker exec ks-test-redis sh -c 'REDISCLI_AUTH="$REDIS_PASSWORD" redis-cli SET kstest:instance 1'
```

Put `KS_TEST_REDIS_URL=redis://:<password>@127.0.0.1:6391` in `.env` next to the personal `KS_REDIS_URL`.

## Running the tests

Redis tests only run against the test instance. Three layers keep them away from the personal one:

1. `ks_core::testkit` refuses a `KS_TEST_REDIS_URL` on port 6390 or on the same endpoint as `KS_REDIS_URL` (localhost
   aliases included), and refuses a server where the key `kstest:instance` is not `1`. The marker is read before any write.
2. `scripts/ks-redis-test.sh [command...]` (default: `cargo test --workspace --locked --no-fail-fast`) refuses port 6390,
   serializes runs across processes with `flock` (`/opt/homebrew/bin/flock` on macOS, util-linux `flock` on CI), and
   checks the marker and the search settings (`search-timeout`, `search-on-timeout`, `search-max-prefix-expansions`)
   before and after the command. A server left in a broken state fails the run.
3. `bash scripts/ks-redis-test-test.sh` tests the wrapper itself.

```sh
bash scripts/ks-redis-test.sh                              # everything
KS_TEST_BROWSER=1 bash scripts/ks-redis-test.sh            # include the real-browser tests
```
