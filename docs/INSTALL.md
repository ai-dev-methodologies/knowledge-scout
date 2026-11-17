# Installation and setup

[README](../README.md) | English | [한국어](ko/INSTALL.md)

Supported platforms: developed and used in practice on macOS. On Linux the full test suite passes in CI (ubuntu), but real-world use has not been verified. Windows is not supported.

## 1. Redis

The search index lives in Redis. Create a `.env` file at the repository root containing `KS_REDIS_PASSWORD=<password>` (to change the port, also set `KS_REDIS_PORT`; the default is 6379), then start Redis. `.env` is not committed to git, so the password never goes into the repository.

```bash
docker compose up -d
```

## 2. Install `ks`

The plugin does not bundle the `ks` executable, so install it first.

```bash
cargo install --git https://github.com/ai-dev-methodologies/knowledge-scout ks-cli
# or, from a cloned repository
cargo install --path crates/ks-cli
```

The installed executable is `ks`. Find its path with `command -v ks` (usually `~/.cargo/bin/ks`). You enter this path when you install the plugin.

## 3. Card data directory and index

Cards and the vocabulary file live in a private directory (`<data>`) outside this repository.

```
<data>/
├── cards/<topic>/<id>.md    # one card = one file
└── vocab/terms.yaml         # vocabulary (terms.yml and terms.json also work)
```

```bash
ks card validate --data <data>      # validate card format
( set -a; . ./.env; set +a; ks index rebuild --data <data> )   # rebuild the Redis index
```

`.env` is loaded only inside a subshell, so the password does not remain in the environment of your interactive shell.

You can also set `KS_REDIS_URL` directly (for example `redis://:<password>@127.0.0.1:6379`). When cards or the vocabulary change, the index must be rebuilt; until then, search stops with an error that points to the rebuild command.

## 4. Install the Claude Code plugin

```
/plugin marketplace add ai-dev-methodologies/knowledge-scout
/plugin install ks@knowledge-scout
```

To install from the shell in one line, run the following.

```bash
claude plugin install ks --marketplace ai-dev-methodologies/knowledge-scout
```

After installing, you must set the three values below before the MCP server starts. Installing with `claude plugin install` from the shell does not prompt for values; it ends with a "3 userConfig options not yet set" notice, so continue with the configuration. Inside Claude Code, run `/plugin configure ks@knowledge-scout`. From the shell, pass JSON on standard input (requires `jq`).

```bash
read -rs KS_REDIS_URL && export KS_REDIS_URL   # enter the Redis URL without leaving it on screen or in shell history
jq -cn --arg ks "$(command -v ks)" --arg data "<data>" \
  '{ks_path: $ks, data_dir: $data, redis_url: env.KS_REDIS_URL}' \
  | claude plugin configure ks@knowledge-scout --values-stdin
```

Do not pass the Redis URL as a command argument such as `--config`; that would leave the password in shell history and the process list.

| Value | Contents |
| --- | --- |
| `ks` executable | The path found in step 2 |
| Card data directory | The `<data>` from step 3 |
| Redis URL | In the form `redis://:<password>@127.0.0.1:6379`. If the password contains characters such as `@`, `:`, `/`, or `%`, percent-encode them |

The Redis URL contains the password, so it is stored in the OS credential store. The other two values are stored in the Claude Code settings.

Verify the connection: check that `plugin:ks:ks` is connected in the `/mcp` list in Claude Code. If you restrict MCP servers with `allowedMcpServers` in your settings, you must allow the ks server as well. Otherwise Claude Code blocks it: the server does not connect and its tools are missing (`claude -p` prints `Warning: MCP server blocked by enterprise policy: <name>` on stderr).

## Skills

- `/ks:search <task description>`: finds candidates in the cards and judges them as A/B/C.
- `/ks:research <topic>`: collects source candidates through Google web research. It runs only when you invoke it directly.
- `/ks:add <URL, file path, or text>`: turns one article into one card. The card key and id are determined with `ks card id`; the skill shows a draft, saves it after you agree, and verifies it with `ks card validate`. Because it handles untrusted sources, this skill pre-allows no tools (whether a confirmation prompt actually appears depends on your Claude Code permission settings). Rebuilding the index requires the Redis password, so run the step 3 command yourself.

## Google login

Web research (`/ks:research`) requires a logged-in Google profile. A person logs in by running `ks auth google` directly in a terminal. It refuses to run when standard input is not a terminal, so an agent or MCP running it as an ordinary command is blocked. It cannot stop a program that runs it with a pseudo-terminal (pty) attached, so only allow the login window for commands you ran yourself.
