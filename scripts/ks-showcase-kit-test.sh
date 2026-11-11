# Self-test of the showcase kit (deploy/showcase, docs/SHOWCASE.md, env.example). Needs `docker compose` and `jq`; starts nothing.
# Run: bash scripts/ks-showcase-kit-test.sh
set -u
here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
d="$here/deploy/showcase"
fail=0
ok()  { echo "ok   $1"; }
bad() { echo "FAIL $1"; fail=1; }
tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
printf 'KS_REDIS_PASSWORD=placeholder\n' > "$tmp/redis.env"
app_env=(KS_BIN=/x KS_DATA=/x KS_UID=1 KS_GID=1 KS_KEYS_DIR=/x)

# 1. redis.yml alone resolves without any app variable
env -u KS_BIN -u KS_DATA -u KS_UID -u KS_GID -u KS_KEYS_DIR docker compose -f "$d/redis.yml" -p ks-kit --env-file "$tmp/redis.env" config >/dev/null 2>&1 \
  && ok "redis.yml alone resolves" || bad "redis.yml alone resolves"

# 2. redis.yml + app.yml without KS_BIN is refused
env -u KS_BIN docker compose -f "$d/redis.yml" -f "$d/app.yml" --env-file "$tmp/redis.env" config >/dev/null 2>&1
[ $? -ne 0 ] && ok "app.yml without KS_BIN refused" || bad "app.yml without KS_BIN refused"

# 3. an empty KS_ALLOWED_HOSTS gives exactly one `--allowed-host=` element
env "${app_env[@]}" KS_ALLOWED_HOSTS= docker compose -f "$d/redis.yml" -f "$d/app.yml" --env-file "$tmp/redis.env" config --format json 2>/dev/null \
  | jq -e '.services.ks.command | (map(select(. == "--allowed-host=")) | length) == 1' >/dev/null \
  && ok "empty allowed host is one argument" || bad "empty allowed host is one argument"

# 4. defaults: loopback bind, port 8790, min role reader, Redis on 127.0.0.1:6391, host network, memory caps
env "${app_env[@]}" docker compose -f "$d/redis.yml" -f "$d/app.yml" --env-file "$tmp/redis.env" config --format json 2>/dev/null > "$tmp/cfg.json"
jq -e '.services.ks.command | index("127.0.0.1:8790") != null and index("reader") != null and (index("--no-auth") == null)' "$tmp/cfg.json" >/dev/null \
  && ok "default bind is loopback:8790, reader, with keys" || bad "default bind is loopback:8790, reader, with keys"
jq -e '.services.ks.network_mode == "host" and .services.ks.environment.KS_REDIS_HOST == "127.0.0.1" and .services.ks.environment.KS_REDIS_PORT == "6391"' "$tmp/cfg.json" >/dev/null \
  && ok "ks reaches Redis over the loopback" || bad "ks reaches Redis over the loopback"
jq -e '.services.redis.ports | all(.host_ip == "127.0.0.1")' "$tmp/cfg.json" >/dev/null \
  && ok "Redis port published on loopback only" || bad "Redis port published on loopback only"
jq -e '(.services.redis.mem_limit | tonumber) <= 268435456 and (.services.ks.mem_limit | tonumber) <= 268435456' "$tmp/cfg.json" >/dev/null \
  && ok "memory caps <= 256m" || bad "memory caps <= 256m"
jq -e '.services.ks.user == "1:1" and .services.ks.volumes[0].read_only == true' "$tmp/cfg.json" >/dev/null \
  && ok "ks runs as KS_UID:KS_GID, binary mounted read-only" || bad "ks runs as KS_UID:KS_GID, binary mounted read-only"

# 5. app.noauth.yml adds --no-auth on loopback only
env "${app_env[@]}" docker compose -f "$d/redis.yml" -f "$d/app.yml" -f "$d/app.noauth.yml" --env-file "$tmp/redis.env" config --format json 2>/dev/null \
  | jq -e '.services.ks.command | index("--no-auth") != null and index("127.0.0.1:8790") != null' >/dev/null \
  && ok "app.noauth.yml: --no-auth on 127.0.0.1" || bad "app.noauth.yml: --no-auth on 127.0.0.1"

# 6. env.example: loopback default, placeholders only
grep -q '^KS_MCP_BIND=127.0.0.1$' "$d/env.example" && grep -q '^KS_ALLOWED_HOSTS=$' "$d/env.example" \
  && ! grep -Eq '^KS_REDIS_PASSWORD=[^<]' "$d/env.example" && ok "env.example defaults and placeholders" || bad "env.example defaults and placeholders"

# 7. the runbook block exists, starts with the positional-argument line, and parses
sh="$tmp/install.sh"
awk '/<!-- runbook:install -->/{f=1;next}/<!-- \/runbook:install -->/{f=0}f' "$here/docs/SHOWCASE.md" 2>/dev/null | sed '/^```/d' > "$sh"
[ -s "$sh" ] && head -1 "$sh" | grep -q '^KIT=${1:?} ENVF=${2:?} P=${3:?} KEYS=${4:?}$' && bash -n "$sh" \
  && ok "runbook install block parses" || bad "runbook install block parses"


# 8. every `compose run` in the block reads /dev/null: the block is fed to `bash -s` on stdin, and a container would swallow the rest of it
awk '/<!-- runbook:install -->/{f=1;next}/<!-- \/runbook:install -->/{f=0}f' "$here/docs/SHOWCASE.md" | grep -F 'dc run' | grep -vqF '</dev/null' \
  && bad "dc run lines read /dev/null" || ok "dc run lines read /dev/null"

exit $fail
