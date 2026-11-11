# 원격 MCP (v0.2.0)

[README](../../README.ko.md) | [English](../REMOTE-MCP.md)

**상태: v0.2.0에 구현됐습니다.** 아래 명령과 옵션은 `ks key --help`, `ks mcp serve --help`와 일치합니다. `--http` 없이는 MCP 서버가 내 컴퓨터에서 stdio로만 동작합니다([설치](INSTALL.md) 참고).

## 무엇인가

같은 `ks` 엔진에 작은 HTTP 모드를 더한 것입니다. 카드가 있는 컴퓨터에 접속하지 않은 사람도, 운영자가 발급한 키로 자기 Claude Code에서 그 카드를 검색할 수 있습니다.

- **검색만 합니다.** 원격 서버는 `ks_search`와 `ks_get` 두 도구만 등록합니다. 웹 검색, 기록, 수집, 카드 만들기, 색인 재생성은 서버의 운영자만 합니다. `ks_search_web`과 `ks_record`는 클라이언트가 직접 호출해도 HTTP로는 쓸 수 없습니다.
- **계정이 아니라 키입니다.** 운영자가 API 키를 발급합니다. 서버는 키의 SHA-256 해시와 역할, 목적, 사용자 표시만 그 인스턴스의 Redis에 저장합니다. 키 원문은 본인만 읽을 수 있는 파일에 한 번만 나옵니다.
- **역할이 보이는 범위를 정합니다.**

  | 역할 | `ks_search`와 `ks_get` |
  | --- | --- |
  | `reader` | `scope: public`인 카드만. `ks_get`은 정해진 필드만 돌려줍니다: `id`, `title`, `summary`, `problems`, `limits`, `evidence_type`, `entities`, `techniques`, `domains`, `topic`, `published_at`. 출처 URL, 출처 경로, 카드 키는 없습니다. |
  | `admin`, `owner` | 출처 필드를 포함한 카드 Markdown 전체 |

- **기본은 닫혀 있습니다.** loopback이 아닌 주소에서는 유효한 키가 하나도 없으면 서버가 시작하지 않고, 그런 주소에서 `--no-auth`도 거부합니다. 키 저장소에 닿지 못하면 요청을 거부하고(HTTP 503) 도구를 실행하지 않습니다. 요청마다 Host와 Origin을 검사합니다.

## 운영자: 키를 발급하고 서버를 띄운다

```bash
# 키 발급. 키 원문은 파일에만 쓰고(권한 0600, 이미 있으면 거부), 화면에는 id만 나옵니다
ks key issue --role reader --purpose <목적> --user <사용자 표시> --out <파일>
ks key list              # id, 역할, 목적. 키와 해시는 보여 주지 않습니다
ks key revoke <id>       # 다음 요청부터 바로 적용됩니다

ks mcp serve --data <data> --http 127.0.0.1:8790 --min-role reader
```

- `--min-role reader|admin|owner`는 서버가 받는 가장 낮은 역할입니다. `scope`가 `public`이 아닌 카드가 하나라도 있는 풀은 `--min-role reader`로 시작하지 않습니다.
- `--no-auth`는 loopback 주소에서만 쓸 수 있고, 모든 호출자에게 `reader` 역할을 줍니다.
- Host 검사: loopback 이름(`localhost`, `127.0.0.1`, `::1`)은 항상 허용하므로 SSH 터널은 추가 설정 없이 됩니다. 다른 이름으로 열려면 `--allowed-host <이름>[,<이름>...]`으로 적습니다(반복 가능, 포트는 붙이지 않습니다). 빈 값은 무시합니다.
- `Origin` 헤더가 있는 요청은 `--allowed-origin`에 적지 않으면 거부합니다. 명령줄 클라이언트는 이 헤더를 보내지 않습니다.
- 요청마다 stderr에 한 줄을 남깁니다: 키 id, 역할, 도구, 지연, 결과 코드. 키 자체는 남기지 않습니다.

## 사용자: 키로 연결한다

운영자의 호스트로 터널을 열고, Claude Code가 로컬 쪽 끝을 보게 합니다. 자리표시자를 바꿔 쓰세요.

```bash
ssh -L 8790:127.0.0.1:8790 <ssh-host>
```

```json
{
  "mcpServers": {
    "ks-remote": {
      "type": "http",
      "url": "http://127.0.0.1:8790/mcp",
      "headers": { "Authorization": "Bearer ${KS_KEY}" }
    }
  }
}
```

키는 `KS_KEY` 환경 변수에 넣습니다(운영자가 준 키 파일에서 읽으세요. 명령줄에 붙여 넣으면 셸 기록에 남습니다). 쓰는 Claude Code 버전이 `headers` 안의 `${VAR}`를 풀어 주는지 확인하세요. 설정에서 `allowedMcpServers`로 MCP 서버를 제한했다면 이 서버도 허용해야 합니다. 허용하지 않으면 Claude Code가 이 서버를 막아 연결되지 않고 도구도 보이지 않습니다(`claude -p`에서는 stderr에 `Warning: MCP server blocked by enterprise policy: <이름>`이 나옵니다).

응답: 키가 없거나 틀리거나 폐기됐으면 401, `--min-role`보다 낮은 역할이거나 허용되지 않은 Host면 403입니다.

## 다루지 않는 것

TLS와 공개 인터넷 엔드포인트는 v0.2.0 범위가 아닙니다. 기본 경로는 위의 SSH 터널입니다. 서버를 직접 노출할지는 운영자가 따로 정할 일입니다.
