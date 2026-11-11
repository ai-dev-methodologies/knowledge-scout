#!/usr/bin/env bash
# Run a command (default: the full workspace test suite) against the test Redis, one run at a time.
#   bash scripts/ks-redis-test.sh [command...]
# - KS_TEST_REDIS_URL comes from the environment or from .env (KS_TEST_ENV_FILE overrides the path); values are never printed.
# - Refuses the personal Redis port (6390). The Rust testkit refuses it again, plus any endpoint equal to KS_REDIS_URL.
# - Redis tests change server settings for a moment, so runs are serialized across processes with flock
#   (macOS: /opt/homebrew/bin/flock; CI: util-linux flock). Lock file: KS_TEST_LOCK (default /tmp/ks-test-redis.lock).
# - Before and after the command the server is checked: marker kstest:instance = 1 and the search settings that `ks`
#   requires (timeout > 0, on-timeout FAIL, prefix expansions >= 1000000). A broken server fails the run.
# The checks use `docker exec` into the container that publishes the test port, so no password is passed on a command line.
set -u
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
die() { echo "ks-redis-test: $*" >&2; exit 2; }

if [ -z "${KS_TEST_REDIS_URL:-}" ]; then
  envf="${KS_TEST_ENV_FILE:-$root/.env}"
  [ -f "$envf" ] && { set -a; . "$envf"; set +a; }
fi
[ -n "${KS_TEST_REDIS_URL:-}" ] || die "KS_TEST_REDIS_URL is not set (and not in .env)"
if [[ "$KS_TEST_REDIS_URL" =~ ^[a-z]+://[^/]*:([0-9]+)(/.*)?$ ]]; then port="${BASH_REMATCH[1]}"; else port=6379; fi
[ "$port" != 6390 ] || die "KS_TEST_REDIS_URL uses the personal Redis port 6390"

if [ -z "${KS_TEST_LOCKED:-}" ]; then
  lock="${KS_TEST_LOCK:-/tmp/ks-test-redis.lock}"
  flock_bin="$(command -v flock)" || die "flock not found (macOS: /opt/homebrew/bin/flock)"
  KS_TEST_LOCKED=1 exec "$flock_bin" -x "$lock" bash "${BASH_SOURCE[0]}" "$@"
fi

rcli() { docker exec "$container" sh -c 'REDISCLI_AUTH="$REDIS_PASSWORD" redis-cli "$@"' sh "$@"; }
check_server() {
  container="$(docker ps -q --filter "publish=$port" | head -1)"
  [ -n "$container" ] || { echo "ks-redis-test ($1): no container publishes port $port" >&2; return 1; }
  [ "$(rcli GET kstest:instance)" = 1 ] || { echo "ks-redis-test ($1): marker kstest:instance is not 1" >&2; return 1; }
  local t o m
  t="$(rcli CONFIG GET search-timeout | sed -n 2p)"; o="$(rcli CONFIG GET search-on-timeout | sed -n 2p)"; m="$(rcli CONFIG GET search-max-prefix-expansions | sed -n 2p)"
  case "$t" in ''|*[!0-9]*|0) echo "ks-redis-test ($1): search-timeout is '$t'" >&2; return 1;; esac
  [ "$(printf '%s' "$o" | tr 'A-Z' 'a-z')" = fail ] || { echo "ks-redis-test ($1): search-on-timeout is '$o'" >&2; return 1; }
  case "$m" in ''|*[!0-9]*) echo "ks-redis-test ($1): search-max-prefix-expansions is '$m'" >&2; return 1;; esac
  [ "$m" -ge 1000000 ] || { echo "ks-redis-test ($1): search-max-prefix-expansions is $m" >&2; return 1; }
}

[ $# -gt 0 ] || set -- cargo test --workspace --locked --no-fail-fast
export KS_TEST_REDIS_URL
check_server before || exit 3
"$@"; rc=$?
check_server after || { [ $rc -ne 0 ] || rc=3; }
exit $rc
