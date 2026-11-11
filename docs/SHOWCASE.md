# Showcase: a sample pool behind the remote MCP

[README](../README.md) | English

A small, repeatable install of the remote MCP (see [REMOTE-MCP](REMOTE-MCP.md)) on a Linux host with Docker: one Redis, one `ks` server holding the 17 public sample cards in `examples/sample-data/`, and API keys. The server binds `127.0.0.1` only; callers reach it through an SSH tunnel. Publishing it on the internet is a separate decision for the operator and is not covered here.

What you need on the host: Docker with the compose plugin, `openssl`, `curl`, at least 5 GB free disk, no sudo. Footprint: about 128 MB of Redis memory, a 256 MB cap per container.

## Kit layout

```
deploy/showcase/redis.yml        Redis (starts alone; named volume redis-data)
deploy/showcase/app.yml          ks-init (index rebuild) and ks (remote MCP, keys required)
deploy/showcase/app.noauth.yml   keyless fallback, loopback only
deploy/showcase/env.example      the variables, placeholders only
examples/sample-data/            cards, vocabulary, golden queries
```

Get the files and the binary onto the host (replace `<ssh-host>`, `<kit>`). The binary is the `ks-linux-x86_64` artifact of a release or `rc/**` CI run; it is a static executable, so the host's libc does not matter.

```bash
git archive <release-or-rc-commit> deploy/showcase examples/sample-data | ssh <ssh-host> 'mkdir -p <kit> && tar -x -C <kit>'
scp ks ks.sha256 <ssh-host>:<kit>/      # from the downloaded artifact; the script checks the hash
```

## Install

The code block below is the whole install. It takes four positional arguments: the kit directory, an env file path, a compose project name, and a keys directory (outside any repository). It creates the env file if it does not exist (mode 0600, random Redis password, default ports 6391 and 8790), starts Redis, issues one `owner` and one `reader` key into the keys directory (mode 0600 files, never printed), builds the index, and starts the server. Run it on the host with a clean shell:

```bash
ssh <ssh-host> env -i HOME=<home> PATH=/usr/local/bin:/usr/bin:/bin bash --noprofile --norc -s -- <kit> <kit>/showcase.env ks-showcase <keys-dir> < install.sh
```

where `install.sh` is the code block below, saved without its fence lines.

<!-- runbook:install -->
```bash
KIT=${1:?} ENVF=${2:?} P=${3:?} KEYS=${4:?}
set -eu
dc() { docker compose --env-file "$ENVF" -p "$P" -f "$KIT/deploy/showcase/redis.yml" -f "$KIT/deploy/showcase/app.yml" "$@"; }

# 1. preflight: free disk and the binary
[ "$(df -BG --output=avail "$KIT" | tail -1 | tr -dc 0-9)" -ge 5 ] || { echo "less than 5G free under $KIT" >&2; exit 1; }
(cd "$KIT" && sha256sum -c ks.sha256)
docker run --rm -v "$KIT/ks:/opt/ks/ks:ro" ubuntu:24.04 /opt/ks/ks --version

# 2. env file (kept if it exists); the Redis password goes straight into the file
if [ ! -e "$ENVF" ]; then
  ( umask 077
    { echo "KS_BIN=$KIT/ks"; echo "KS_DATA=$KIT/examples/sample-data"
      echo "KS_UID=$(id -u)"; echo "KS_GID=$(id -g)"; echo "KS_KEYS_DIR=$KEYS"
      echo "KS_REDIS_PORT=6391"; echo "KS_REDIS_PASSWORD=$(openssl rand -hex 24)"
      echo "KS_MCP_BIND=127.0.0.1"; echo "KS_MCP_PORT=8790"; echo "KS_MIN_ROLE=reader"; echo "KS_ALLOWED_HOSTS="
    } > "$ENVF" )
fi

# 3. Redis, then keys (the key store lives in Redis), then the index, then the server
mkdir -p "$KEYS" && chmod 700 "$KEYS"
dc up -d --wait redis
[ -e "$KEYS/owner.key" ] || dc run --rm -T --no-deps -v "$KEYS:/keys" ks /opt/ks/ks key create --role owner --purpose showcase-owner --user operator --out /keys/owner.key </dev/null
[ -e "$KEYS/reader.key" ] || dc run --rm -T --no-deps -v "$KEYS:/keys" ks /opt/ks/ks key create --role reader --purpose showcase-reader --user guest --out /keys/reader.key </dev/null
dc run --rm -T ks-init </dev/null
dc up -d --wait ks
dc ps
```
<!-- /runbook:install -->

Keys live in the project's Redis volume. If you remove the volume (`down -v`), delete the key files too, or the files no longer match anything.

## Check

On the host, both ports must be loopback-only (`netstat -ltn` shows `127.0.0.1:6391` and `127.0.0.1:8790`) and both containers healthy (`docker compose ... ps`).

From your machine, open the tunnel and call the server (the key is read from a file; do not paste it into a command line):

```bash
ssh -N -L 8790:127.0.0.1:8790 <ssh-host> &
curl -s -o /dev/null -w '%{http_code}\n' -X POST http://127.0.0.1:8790/mcp -H 'content-type: application/json' -H 'accept: application/json, text/event-stream' -d '{"jsonrpc":"2.0","id":1,"method":"tools/list"}'   # 401 without a key
```

With a valid key, `tools/list` returns exactly `ks_get` and `ks_search`; a `reader` key gets `ks_get` results without `source_url`, `source_path`, or `card_key`. See [showcase/demo-prompt.md](showcase/demo-prompt.md) for the two-query run from Claude Code and [showcase/mcp.json.example](showcase/mcp.json.example) for the client entry. If your Claude Code restricts servers with `allowedMcpServers`, add `{"serverUrl": "http://127.0.0.1:8790/mcp"}` there.

## Operate

```bash
dc() { docker compose --env-file <kit>/showcase.env -p ks-showcase -f <kit>/deploy/showcase/redis.yml -f <kit>/deploy/showcase/app.yml "$@"; }
dc down          # stop; the volume and the keys are kept
dc up -d --wait  # start again, no new keys
dc down -v       # remove everything including the keys' Redis records
```

Revoke a key with `dc run --rm -T --no-deps ks /opt/ks/ks key revoke <id>`; `key list` shows ids, roles, and purposes.

Keyless fallback (loopback only, every caller is a `reader`): add `-f <kit>/deploy/showcase/app.noauth.yml` to every `dc` call.

To serve names other than `localhost`, `127.0.0.1`, and `::1` (for example a private network address), set `KS_ALLOWED_HOSTS` in the env file and `KS_MCP_BIND` to that address; the server then refuses to start unless a key exists.
