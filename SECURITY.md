# Security

[README](README.md) | English | [한국어](docs/ko/SECURITY.md)

## Public/private boundary

- **Public**: the engine code in this repository (`ks-core`, `ks-cli`, `ks-browser`).
- **Private**: card `.md` files, source articles, browser profiles (login cookies), and the application log are not kept in this repository; they live in each user's own data directory. Cards use the user's private git repository as the source of truth. A card records only the path and hash of its source article.
- **Personal vs. team**: separated by the card frontmatter fields `scope` (`private` / `team` / `public`) and `access` (`A` / `B` / `unknown`). By design, the only cards shared with a team are those with `scope=team` and `access=A`, and private fields such as URL and author are stripped. The export command (`ks export`) and the PR/CI process of the team repository do not exist yet (planned). For now, only recording the two fields on the card is implemented.

## Security design summary

Only what can be confirmed in code is listed.

- **Daemon IPC**: The daemon accepts requests only over a Unix socket (`0600`, parent `run/` directory `0700`). Every request must carry a 256-bit random token (a `0600` file) that is created fresh at each start; otherwise the response is `unauthorized`. The client also reads the token file only if it is a regular file owned by the current user with no group or other permissions. A file lock per profile ensures only one daemon runs.
- **CDP over a pipe, no TCP port**: Chrome is launched with `--remote-debugging-pipe` and communicates only over file descriptors 3 and 4, so no other local process can attach to the browser.
- **Cookie protection**: The keychain-bypass flags (`--password-store=basic`, `--use-mock-keychain`) are not used, so in both headless and headful mode Chrome encrypts cookies with a key from the OS keychain as it does by default. A test (`chrome_args`) checks that these flags are absent. With the installed Google Chrome on macOS, it uses the key of the same keychain entry (`Chrome Safe Storage`) as your personal Chrome. On Linux, desktop session variables are passed so that Chrome can use the secret store and a window (see the allowlist below). This behavior has not been verified on Linux yet.
- **Child process environment allowlist**: The daemon and Chrome start with a cleared environment and receive only `HOME`, `PATH`, `TMPDIR`, `LANG`, the Linux desktop session variables (`DISPLAY`, `WAYLAND_DISPLAY`, `XAUTHORITY`, `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, `XDG_CURRENT_DESKTOP`), and `KS_*` variables whose names do not contain `REDIS`. The Redis password and URL are not passed on.
- **No detection evasion**: No stealth flags are added, and CAPTCHAs are neither solved nor bypassed. Login is also done by a person, and `ks` never sees the password. The login window (`ks auth google`) is a regular Chrome launched without an automation connection (CDP), and the search browser does not hide its automation markers.
- **A person opens the login window**: If standard input is not a terminal, `ks auth google` refuses without changing the daemon, the profile, or Chrome. It cannot stop a program that runs it with a pseudo-terminal attached.
- **What is not logged**: The daemon log never contains tokens, URLs, query text, or page content. `events.log` (`0600`) also never contains query text, cookies, or account names.
- **Redis**: The default connection address is `127.0.0.1`, and `docker-compose.yml` likewise opens the port only on `127.0.0.1` and requires a password (`.env`, which is not included in the repository).
