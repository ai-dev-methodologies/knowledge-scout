# knowledge-scout 구조

업무를 설명하면 개인이나 팀이 쌓아 둔 기술 지식 카드에서 **우선순위가 붙은 후보**를 찾아 주는 도구입니다. 엔진(이 저장소)은 공개하고, 카드와 원문은 각자의 비공개 데이터 디렉터리에 둡니다. 실행은 사용자가 구독 중인 AI 세션(Claude Code) 안에서 스킬, 명령어, MCP로 합니다.

## 1. 구성요소

```
┌──────────── AI 세션 (Claude Code) ────────────┐
│  스킬 /ks:search  /ks:add  /ks:research       │  ← 절차와 판정 규칙(문서)
│        │ MCP 호출                              │
└────────┼──────────────────────────────────────┘
         ▼
   ks MCP 서버 (Rust, rmcp, stdio — `ks mcp serve`)
     ks_search     후보 수집: 어휘 정규화 → 용어·텍스트 일치 → 정렬
     ks_get        카드 상세
     ks_record     적용 기록(채택·기각·결과)
     ks_search_web 수집 어댑터 호출 (프로필은 default 고정, needs_human 이벤트를 그대로 전달)
         │
         ├── 색인: Redis (필수, local·shared 모두)          ← 카드 .md에서 재생성
         ├── 카드 저장소: <data>/cards/<topic>/<id>.md     ← 원본(git, private)
         ├── 어휘 사전: <data>/vocab/terms.yaml
         └── 적용 기록: Redis Stream → <out>/applications.jsonl 덤프(`ks record dump --out <dir>`)
   ks CLI  (같은 코어를 명령행에서)
     ks auth google | ks auth logout --yes | ks browser connect/disconnect | ks handoff | ks resume | ks status
     ks research search --engine google-browser --mode headless|headful
     ks card id | ks card import | ks card validate | ks index rebuild | ks record dump   (/ks:add가 ks card id로 카드 id를 정함)
     계획: ks export --scope team
   수집 어댑터
     google-browser (기본, Rust 데몬 + agent-browser 유래 CDP 클라이언트 + gstack식 handoff/resume)
     계획: google-api (키가 있을 때)   ai-websearch (대체 경로)   비공개 수집기 어댑터(저장소 밖)
```

## 2. 데이터 (카드 1장 = .md 1개)

- **frontmatter**: `id`(정규화한 출처 키 `card_key`의 sha256 앞 16자리), `source_url`, `scope`(private / team), `access`(A / B / unknown), `evidence_type`, `published_at`, `collected_at`, 어휘 용어(`entities` / `techniques` / `domains`), `problems`
- **본문**: 3줄 요약, 한계
- **원문**: 저장소 밖 개인 보관소에 둔다. 카드에는 경로와 해시만 적는다.
- **팀으로 내보내기(계획)**: `scope=team`이고 `access=A`인 카드만 내보낸다. 비공개 필드(URL, 저자 등)는 뺀다.

## 3. 검색 흐름 (평가로 정한 방식, 2026-11-10)

1. **질의 해석** (세션 모델): 업무 설명에서 어휘 용어와 한국어 문제 구절을 뽑는다.
2. **후보 수집** (`ks_search`): 다음 순서로 정렬해 상위 40장을 고른다.
   1. 일치한 용어 수
   2. 텍스트 일치 수
   3. 드문 용어
   4. id

   결과에는 검색 범위(`l1_cards`: 검색 대상 카드 수)를 함께 적는다. 어휘 사전에 없는 질의 용어는 `unknown_terms`에 담아 돌려주므로, 사전에 없는 용어가 조용히 0건이 되지 않는다.

   Redis 색인은 `ks index rebuild`가 읽은 어휘 사전 파일과 카드 파일의 지문(`vocab_fp`, `cards_fp`)을 meta에 함께 적는다. `ks search`와 `ks_search`는 검색 직전에 지금의 지문과 비교해, 다르거나 지문이 없는 옛 색인이면 검색하지 않고 `ks index rebuild --data <dir> --vocab <file>`을 안내한다. `ks mcp serve`는 오래 떠 있으므로 검색마다 비교한다. 카드 지문은 파일 목록과 각 파일의 크기·수정 시각만 보므로, 같은 크기로 같은 시각에 덮어쓴 변경은 잡지 못한다. 테스트용 file 백엔드는 비교하지 않는다. rebuild는 시작하자마자 meta를 지우고 끝날 때 새 `build_id`와 함께 쓴다. 검색은 끝난 뒤 meta를 다시 읽어, 그 사이 rebuild가 있었으면 결과를 버리고 다시 검색하라는 오류를 낸다. rebuild는 `<prefix>rebuild-lock`(SET NX, 15분 만료, 토큰 비교 후 해제)을 잡고 실행되어 동시에 하나만 돈다.
3. **판정** (세션 모델, 카드만 읽음): A는 같은 문제, B는 수단 차용, C는 분야만 겹침, 나머지는 제외한다. 카드의 한계 문구를 함께 보여 주고, 조건이 맞는지는 사람이 확인한다.
4. **방향 제시와 적용 기록** (`ks_record`)

## 4. 사람 개입 이벤트

`AUTH_REQUIRED` / `AUTH_EXPIRED` / `CAPTCHA` / `CONSENT` / `MFA_PROMPT` / `BLOCKED` / `ENGINE_SWITCH` / `DAEMON_REPLACE`

이벤트가 생기면 다음 순서로 처리한다.
1. 작업을 즉시 멈춘다.
2. `events.log`에 기록한다.
3. 알린다. MCP 응답으로 `needs_human`과 처리 절차를 돌려주고, macOS 데스크톱 알림을 띄운다.
4. 사람이 처리한 뒤 `ks resume`으로 이어 간다. 로그인 계열은 `ks auth google`로 다시 로그인해서 이어 간다.

멈춘 이벤트는 `BLOCKED`를 포함해 모두 미처리 상태로 남고, `ks resume`(로그인 계열은 `ks auth google`)이 해결된 것을 확인할 때까지 다음 검색을 막는다. 로그인 계열(`AUTH_REQUIRED` / `AUTH_EXPIRED` / `MFA_PROMPT`)은 `ks auth google`로 푼다. Google은 CDP로 조종되는 창에서 로그인을 막으므로, 이 명령은 데몬 브라우저를 닫고 조종 연결이 없는 일반 Chrome 창을 같은 프로필로 띄운다. 이때 같은 프로필에서 사람이 쓰던 데몬 창(예: CAPTCHA를 풀던 창)도 닫힌다. 표준 입력이 터미널이 아니면(에이전트나 MCP가 `resume_cmd`를 실행한 경우) 데몬, 프로필, Chrome 어느 것도 바꾸지 않고 거부한다. 사람이 로그인하고 Enter를 누르면 그 Chrome을 SIGTERM으로 정상 종료시킨 뒤, headless 브라우저에서 로그인된 Google 계정 페이지가 보일 때 푼다. 창에 로그인된 Google 계정 페이지가 보이면 `ks resume`으로도 풀린다. 나머지는 정상적인 Google 결과 페이지가 보일 때만 풀린다. 오류 페이지나 재시작 뒤의 빈 페이지에서는 풀지 않고, 사람이 할 일을 안내한다. 창에 다른 문제(예: 동의 페이지)가 보이면 그것을 먼저 처리하라고 알리되, 미처리 이벤트를 그것으로 바꾸지 않고 원래 해제 조건을 그대로 둔다. 미처리 이벤트는 `run/<profile>.pending`(0600)에 저장되어 데몬을 다시 띄워도 이어진다. 미처리 이벤트가 있거나 창이 실제로 떠 있는(headful) 동안에는 유휴 시간이 지나도 데몬을 끄지 않는다. 창을 닫거나 Chrome이 죽으면 데몬은 브라우저가 없는 상태로 돌아간다. 다음 `connect`나 검색이 브라우저를 새로 띄우고, 그동안은 유휴 종료 대상이 된다. `ping`(`ks status`)은 유휴 시간을 늘리지 않는다.

같은 이벤트가 검색 3번 연속으로 나오면 `BLOCKED`로 올린다. 그 사이에 `ks resume`을 했어도 센다. 정상 검색이 한 번 끼거나 다른 이벤트가 나오면 처음부터 다시 센다.

검색 횟수에는 프로필별 한도(쿼터)가 있다. 기본값은 1시간에 20회, 24시간에 100회이고, 데몬을 시작할 때 `KS_QUOTA_HOURLY`와 `KS_QUOTA_DAILY`로 바꾼다. 데몬은 Google에 질의를 보내기 직전에 횟수를 `run/<profile>.quota`(0600)에 적는다. 모드 전환이나 handoff 때 이전 페이지를 다시 여는 것도 검색과 같은 관문을 지난다. 한도를 확인하고, 질의 사이 3~8초 간격을 지키고, 횟수를 적는다. 한도를 넘으면 다시 열지 않고 응답의 `note`로 알린다. quota 파일이 없으면 기록이 없는 것으로 본다. 파일이 있는데 읽을 수 없으면(UTF-8이 아님, 심볼릭 링크, 권한 없음, 빈 줄이 아닌데 타임스탬프가 아닌 줄이 있음) 검색을 거부하고 조치를 안내한다. 모든 검색이 프로필당 데몬 하나를 거치므로 CLI와 MCP가 이 한도를 함께 쓴다. 한도를 넘으면 브라우저를 띄우지 않고 `needs_human` `BLOCKED`(사유가 `quota`로 시작)를 돌려주며, 안내에는 Google에 아무 요청도 보내지 않았다고 적는다. 시간이 지나면 풀리므로 미처리 상태로는 남기지 않는다. CLI의 `--max-queries`는 한 번에 최대 50이다.

브라우저 프로필의 쿠키(Google 로그인 세션 포함)는 headless와 headful 모두에서 OS 키체인 키로 암호화된다. macOS는 Keychain, Linux는 libsecret/kwallet을 쓰고, 둘 다 없으면 Chrome 기본 동작을 따른다. 로그인 창에서 저장한 쿠키를 headless가 읽는지는 `ks auth google`이 창을 닫은 뒤 headless 브라우저로 로그인된 Google 계정 페이지를 열어 자동으로 확인한다(`auth_check`). 고정 키 플래그(`--password-store=basic`, `--use-mock-keychain`)를 쓰던 시절에 만든 프로필은 쿠키를 읽을 수 없으므로 다시 로그인해야 한다.

이벤트별 조건과 절차는 이 절과 README의 "Google 로그인" 절을 본다.

## 5. 모드

- **local**: 내 카드 디렉터리와 localhost Redis(필수)를 쓴다. PR 절차는 없다.
- **shared(계획)**: 팀 private GitHub repo의 카드를 PR로 받는다. CI 검사를 거쳐 머지하고, 팀 색인에 반영한다. 검색할 때 결과에 개인/팀 출처를 표시한다.

## 6. 배포

- Claude Code plugin으로 배포한다. 스킬(`/ks:search`, `/ks:research`, `/ks:add`)과 MCP 서버 등록을 묶는다. `ks` 실행 파일은 plugin에 넣지 않고 사용자가 따로 설치한다. plugin은 설치할 때 `userConfig`로 `ks` 경로, 카드 데이터 디렉터리, Redis URL(비밀값)을 받아 MCP 서버의 `command`, `args`, `env`에 쓴다.
- 사람 개입(handoff/resume)과 이벤트 설계는 gstack browse(MIT)를 참고했고 코드는 가져오지 않았다. 데몬과 CDP 클라이언트는 아래 agent-browser를 기준으로 한다. 탐지 회피 기능은 넣지 않는다.
- Google 브라우저 수집은 약관 위험이 있다. 기본값은 낮은 빈도와 사람 개입 우선으로 두고, README에 명시한다.

## 7. 저장소 밖 (이 저장소에 넣지 않는 것)

- 개인 원문, 카드, 브라우저 프로필(자격증명), 비공개 수집기 어댑터
- (계획) pre-push 훅과 CI가 이런 경로와 패턴을 거부한다. 지금은 `ks card validate`의 텍스트 검사만 있다.

## 8. 구현 언어 (2026-11-10 확정)

- **Rust**: `ks` CLI, MCP 서버, 카드·어휘·후보 수집 코어, Redis 색인. 크레이트는 `crates/ks-core`(lib), `crates/ks-cli`(bin), `crates/ks-browser`(lib) 세 개다.
- **브라우저도 Rust**: 데몬과 CDP 클라이언트 구조는 vercel-labs/agent-browser(Apache-2.0)를 기준으로 삼고, CDP 클라이언트를 고지와 함께 옮겨 온다. 사람 개입(handoff/resume)과 이벤트는 gstack 방식으로 직접 만든다. Node는 쓰지 않는다.
- **실행 규칙**: Rust는 컴파일된 실행 파일이라 shebang을 쓰지 않는다.
