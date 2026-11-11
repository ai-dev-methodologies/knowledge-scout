#!/usr/bin/env bash
# Self-test of scripts/ks-redis-test.sh. Needs the test Redis (KS_TEST_REDIS_URL, e.g. from .env). Run: bash scripts/ks-redis-test-test.sh
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
w="$here/ks-redis-test.sh"
if [ -z "${KS_TEST_REDIS_URL:-}" ] && [ -f "$here/../.env" ]; then set -a; . "$here/../.env"; set +a; fi
: "${KS_TEST_REDIS_URL:?KS_TEST_REDIS_URL is not set}"
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
export KS_TEST_LOCK="$tmp/lock"
fail=0
ok()  { echo "ok   $1"; }
bad() { echo "FAIL $1"; fail=1; }

# 1. the personal port is refused and the command never runs
KS_TEST_REDIS_URL="redis://:x@127.0.0.1:6390" bash "$w" touch "$tmp/ran-personal" >/dev/null 2>&1
{ [ $? -ne 0 ] && [ ! -e "$tmp/ran-personal" ]; } && ok "personal port refused" || bad "personal port refused"

# 2. a missing URL is refused
env -u KS_TEST_REDIS_URL KS_TEST_ENV_FILE=/nonexistent bash "$w" touch "$tmp/ran-nourl" >/dev/null 2>&1
{ [ $? -ne 0 ] && [ ! -e "$tmp/ran-nourl" ]; } && ok "missing URL refused" || bad "missing URL refused"

# 3. the command's exit code is passed through
bash "$w" sh -c 'exit 7' >/dev/null 2>&1; [ $? -eq 7 ] && ok "exit code passed through" || bad "exit code passed through"

# 4. two concurrent runs do not overlap
for i in 1 2; do
  bash "$w" sh -c "echo start-$i >> '$tmp/order'; sleep 1; echo end-$i >> '$tmp/order'" >/dev/null 2>&1 &
done
wait
if [ "$(wc -l < "$tmp/order" | tr -d ' ')" = 4 ] && [ "$(sed -n 1p "$tmp/order" | sed 's/start-//')" = "$(sed -n 2p "$tmp/order" | sed 's/end-//')" ]; then ok "runs are serialized"; else bad "runs are serialized: $(tr '\n' ' ' < "$tmp/order")"; fi

# 5. the search config is checked after the command: a command that breaks it makes the run fail
# (skipped when the test server is not a local docker container we may reconfigure)
if [[ "$KS_TEST_REDIS_URL" =~ ^[a-z]+://[^/]*:([0-9]+)(/.*)?$ ]]; then port="${BASH_REMATCH[1]}"; else port=6379; fi
c="$(docker ps -q --filter "publish=$port" | head -1)"
if [ -n "$c" ]; then
  bash "$w" sh -c "docker exec $c sh -c 'REDISCLI_AUTH=\"\$REDIS_PASSWORD\" redis-cli CONFIG SET search-timeout 0 >/dev/null'" >/dev/null 2>&1
  rc=$?
  docker exec "$c" sh -c 'REDISCLI_AUTH="$REDIS_PASSWORD" redis-cli CONFIG SET search-timeout 10000 >/dev/null'
  [ $rc -ne 0 ] && ok "post-run config check fails the run" || bad "post-run config check fails the run"
fi

[ $fail -eq 0 ] && echo "ks-redis-test-test: all ok" || { echo "ks-redis-test-test: FAILED"; exit 1; }
