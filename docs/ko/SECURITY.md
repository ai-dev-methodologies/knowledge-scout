# 보안

[README](../../README.ko.md) | [English](../../SECURITY.md) | 한국어

## 공개와 비공개의 경계

- **공개**: 이 저장소의 엔진 코드(`ks-core`, `ks-cli`, `ks-browser`)입니다.
- **비공개**: 카드 `.md`, 원문, 브라우저 프로필(로그인 쿠키), 적용 기록은 이 저장소에 두지 않고 각자의 데이터 디렉터리에 둡니다. 카드는 사용자의 private git 저장소를 원본으로 삼습니다. 카드에는 원문의 경로와 해시만 적습니다.
- **개인과 팀 구분**: 카드 frontmatter의 `scope`(`private` / `team`)와 `access`(`A` / `B` / `unknown`) 필드로 나눕니다. 설계상 팀으로 공유하는 카드는 `scope=team`이고 `access=A`인 것뿐이며 URL과 저자 같은 비공개 필드는 뺍니다. 이 내보내기 명령(`ks export`)과 팀 저장소의 PR·CI 절차는 아직 없습니다(계획). 지금은 두 필드를 카드에 기록하는 데까지만 구현되어 있습니다.

## 보안 설계 요약

코드로 확인되는 것만 적습니다.

- **데몬 IPC**: 데몬은 Unix 소켓(`0600`, 상위 `run/` 디렉터리 `0700`)으로만 요청을 받습니다. 모든 요청은 시작할 때마다 새로 만드는 256비트 무작위 토큰(`0600` 파일)을 실어야 하고, 맞지 않으면 `unauthorized`입니다. 클라이언트도 토큰 파일이 본인 소유의 일반 파일이고 그룹·다른 사용자 권한이 없을 때만 읽습니다. 프로필마다 파일 잠금으로 데몬은 하나만 뜹니다.
- **CDP는 TCP 포트 없이 pipe**: Chrome은 `--remote-debugging-pipe`로 띄워 파일 디스크립터 3, 4로만 통신하므로 다른 로컬 프로세스가 브라우저에 붙을 수 없습니다.
- **쿠키 보호**: 키체인 우회 플래그(`--password-store=basic`, `--use-mock-keychain`)를 쓰지 않으므로 headless와 headful 모두 Chrome 기본 동작대로 OS 키체인의 키로 쿠키를 암호화합니다. 이 플래그가 없는지는 테스트(`chrome_args`)가 확인합니다. macOS에서 설치된 Google Chrome을 쓰면 개인 Chrome과 같은 키체인 항목(`Chrome Safe Storage`)의 키를 씁니다. Linux에서는 Chrome이 시크릿 저장소와 창을 쓸 수 있도록 데스크톱 세션 변수를 넘깁니다(아래 허용 목록). 이 동작은 Linux에서 아직 검증하지 않았습니다.
- **자식 프로세스 환경변수 허용 목록**: 데몬과 Chrome은 환경을 비운 채 `HOME`, `PATH`, `TMPDIR`, `LANG`, Linux 데스크톱 세션 변수(`DISPLAY`, `WAYLAND_DISPLAY`, `XAUTHORITY`, `XDG_RUNTIME_DIR`, `DBUS_SESSION_BUS_ADDRESS`, `XDG_CURRENT_DESKTOP`), 그리고 이름에 `REDIS`가 들어가지 않는 `KS_*`만 받습니다. Redis 비밀번호와 URL은 넘어가지 않습니다.
- **탐지 회피 없음**: 탐지 회피(stealth) 플래그를 넣지 않고, CAPTCHA를 풀거나 우회하지 않습니다. 로그인도 사람이 직접 하며 `ks`는 비밀번호를 보지 않습니다. 로그인 창(`ks auth google`)은 조종 연결(CDP) 없이 띄우는 일반 Chrome이고, 검색 브라우저는 자동화 표시를 숨기지 않습니다.
- **로그인 창은 사람이 연다**: `ks auth google`은 표준 입력이 터미널이 아니면 데몬, 프로필, Chrome을 바꾸지 않고 거부합니다. 의사 터미널을 붙여 실행하는 프로그램까지 막지는 못합니다.
- **기록에 남기지 않는 것**: 데몬 로그에는 토큰, URL, 질의문, 페이지 내용을 쓰지 않습니다. `events.log`(`0600`)에도 질의문과 쿠키, 계정명을 쓰지 않습니다.
- **Redis**: 기본 접속 주소는 `127.0.0.1`이고, `docker-compose.yml`도 `127.0.0.1`에만 포트를 열며 비밀번호(`.env`, 저장소에 포함되지 않음)를 요구합니다.
