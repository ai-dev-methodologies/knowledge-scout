# 설치와 설정

[README](../../README.ko.md) | [English](../INSTALL.md) | 한국어

지원 플랫폼: macOS에서 개발하고 실제로 써 봤습니다. Linux는 CI(ubuntu)에서 전체 테스트가 통과하지만 실제 사용은 확인하지 않았습니다. Windows는 지원하지 않습니다.

## 1. Redis

검색 색인은 Redis에 둡니다. 저장소 루트에 `.env` 파일을 만들어 `KS_REDIS_PASSWORD=<비밀번호>`를 적고(포트를 바꾸려면 `KS_REDIS_PORT`도 적습니다. 기본값은 6379), Redis를 띄웁니다. `.env`는 git에 올라가지 않으며, 비밀번호를 저장소에 넣지 않습니다.

```bash
docker compose up -d
```

## 2. `ks` 설치

plugin은 `ks` 실행 파일을 포함하지 않으므로 먼저 설치해야 합니다.

```bash
cargo install --git https://github.com/ai-dev-methodologies/knowledge-scout ks-cli
# 또는 클론한 저장소에서
cargo install --path crates/ks-cli
```

설치되는 실행 파일은 `ks`입니다. 경로는 `command -v ks`로 확인합니다(보통 `~/.cargo/bin/ks`). plugin을 설치할 때 이 경로를 입력합니다.

## 3. 카드 데이터 디렉터리와 색인

카드와 어휘 사전은 이 저장소 밖의 비공개 디렉터리(`<data>`)에 둡니다.

```
<data>/
├── cards/<topic>/<id>.md    # 카드 1장 = 파일 1개
└── vocab/terms.yaml         # 어휘 사전 (terms.yml, terms.json도 가능)
```

```bash
ks card validate --data <data>      # 카드 형식 검증
( set -a; . ./.env; set +a; ks index rebuild --data <data> )   # Redis 색인 재생성
```

`.env`는 서브셸 안에서만 불러옵니다. 비밀번호가 대화형 셸의 환경변수에 남지 않게 하기 위해서입니다.

`KS_REDIS_URL`(예: `redis://:<비밀번호>@127.0.0.1:6379`)을 직접 설정해도 됩니다. 카드나 어휘 사전이 바뀌면 색인을 다시 만들어야 하며, 그전에는 검색이 오류로 멈추고 재생성 명령을 안내합니다.

## 4. Claude Code plugin 설치

```
/plugin marketplace add ai-dev-methodologies/knowledge-scout
/plugin install ks@knowledge-scout
```

셸에서 한 줄로 설치하려면 다음을 실행합니다.

```bash
claude plugin install ks --marketplace ai-dev-methodologies/knowledge-scout
```

설치한 뒤 아래 세 값을 설정해야 MCP 서버가 뜹니다. 셸에서 `claude plugin install`로 설치하면 값을 묻지 않고 "3 userConfig options not yet set" 안내로 끝나니 이어서 설정합니다. Claude Code 안에서는 `/plugin configure ks@knowledge-scout`를 실행합니다. 셸에서는 JSON을 표준 입력으로 넘깁니다(`jq` 필요).

```bash
read -rs KS_REDIS_URL && export KS_REDIS_URL   # Redis URL을 화면과 셸 기록에 남기지 않고 입력
jq -cn --arg ks "$(command -v ks)" --arg data "<data>" \
  '{ks_path: $ks, data_dir: $data, redis_url: env.KS_REDIS_URL}' \
  | claude plugin configure ks@knowledge-scout --values-stdin
```

Redis URL을 `--config` 같은 명령 인자로 넘기면 비밀번호가 셸 기록과 프로세스 목록에 남으니 쓰지 않습니다.

| 값 | 내용 |
| --- | --- |
| `ks` 실행 파일 | 2단계에서 확인한 경로 |
| 카드 데이터 디렉터리 | 3단계의 `<data>` |
| Redis URL | `redis://:<비밀번호>@127.0.0.1:6379` 형식. 비밀번호에 `@`, `:`, `/`, `%` 같은 문자가 있으면 퍼센트 인코딩합니다 |

Redis URL은 비밀번호를 포함하므로 OS 자격 증명 저장소에 저장됩니다. 나머지 두 값은 Claude Code 설정에 저장됩니다.

연결 확인: Claude Code의 `/mcp` 목록에서 `plugin:ks:ks`가 연결됐는지 봅니다. 설정의 `allowedMcpServers`로 MCP 서버를 제한하고 있다면 ks 서버도 허용해야 합니다. 허용하지 않으면 Claude Code가 이 서버를 막아 연결되지 않고 도구도 보이지 않습니다(`claude -p`에서는 stderr에 `Warning: MCP server blocked by enterprise policy: <이름>`이 나옵니다).

## 스킬

- `/ks:search <업무 설명>`: 카드에서 후보를 찾아 A/B/C로 판정합니다.
- `/ks:research <주제>`: Google 웹 조사로 자료 후보를 모읍니다. 직접 호출할 때만 실행됩니다.
- `/ks:add <URL, 파일 경로, 또는 글>`: 글 한 편을 카드 1장으로 만듭니다. 카드 키와 id는 `ks card id`로 정하고, 초안을 보여 주고 동의를 받은 뒤 저장하고 `ks card validate`로 검증합니다. 신뢰할 수 없는 출처를 처리하므로 이 스킬은 어떤 도구도 미리 허용하지 않습니다(실제로 확인 창이 뜨는지는 Claude Code 권한 설정에 따릅니다). 색인 재생성은 Redis 비밀번호가 필요하므로 3단계의 명령을 직접 실행합니다.

## Google 로그인

웹 조사(`/ks:research`)에는 로그인된 Google 프로필이 필요합니다. 터미널에서 사람이 직접 `ks auth google`을 실행해 로그인합니다. 표준 입력이 터미널이 아니면 거부하므로 에이전트나 MCP가 일반 명령으로 실행하면 막힙니다. 의사 터미널(pty)을 붙여 실행하는 프로그램까지 막지는 못하니, 로그인 창은 직접 실행한 것만 허용하세요.
