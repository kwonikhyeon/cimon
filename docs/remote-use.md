# 외부에서 cimon 쓰기

팀원 PC 에서 — 사무실 밖에서도 — Claude Code 로 cimon 을 불러 빌드·테스트하고 산출물을 받는 방법이다. 빌드 머신을 올리는 쪽은 [setup.md](setup.md).

## 어떻게 닿나

```
팀원 PC ──(tailnet, WireGuard)──> https://cimon.<tailnet>.ts.net
                                    tailscale serve (HTTPS)
                                      └─> 127.0.0.1:8770  cimon (MCP)
```

- cimon 은 **인터넷에 열려 있지 않다.** 회사 tailnet 에 로그인한 기기에서만 닿는다 — 사무실 밖이면 Tailscale 이 켜져 있어야 한다.
- 막는 것은 두 겹이다. 네트워크는 Tailscale 기기 인증과 ACL(`group:cimon-users` 만 443), 서버는 **사용자별 토큰**.
- 빌드는 빌드 머신에서 돈다. 내 PC 에는 툴체인이 없어도 된다.

## 준비물

| 무엇 | 누구에게 |
|---|---|
| tailnet 계정, `group:cimon-users` 등록 | tailnet 관리자 |
| **초대 코드**(`XXXX-XXXX`, 10분·한 번) | cimon 관리자(`cimon invite <이름>` — 기기마다 이름 하나) |
| Claude Code **v2.1.304 이상**(MCP tasks) | 직접 — `claude --version` |

## 빠른 연결 (권장)

Tailscale 에 로그인한 뒤, 서버가 직접 내주는 스크립트를 한 줄로 실행하고 초대 코드를 입력한다.

```powershell
# Windows (PowerShell)
irm https://cimon.<tailnet>.ts.net/connect.ps1 | iex
```
```sh
# macOS · Linux
curl -fsSL https://cimon.<tailnet>.ts.net/connect.sh | sh
```

스크립트가 하는 일:

1. 초대 코드를 서버에 내고 이 기기용 토큰을 받는다 — 토큰은 화면에도 메신저에도 나오지 않는다.
2. 토큰을 Windows 는 DPAPI 파일(`%USERPROFILE%\.cimon\token.dpapi`), macOS 는 키체인, Linux 는 `secret-tool`(없으면 권한 600 파일)에 둔다.
3. 헤더 도우미(`header.ps1` / `header.sh`)를 쓰고 `claude mcp add-json --scope user cimon …` 로 등록한다(이전 등록은 바꾼다).
4. `claude mcp get cimon` 으로 등록 내용과 연결(Connected)을 확인한다 — 어긋나면 「done」 대신 실패로 끝난다.

`done - signed in as <이름>` 이 나오면 「3. 확인」 으로 간다. 코드는 대소문자·`-` 를 가리지 않고, `O`/`0`·`I`/`1` 을 헷갈려도 된다. 자동 설치 스크립트에서는 코드를 `CIMON_INVITE_CODE` 환경 변수로 넘기면 묻지 않는다.

손으로 등록하려면 아래 1·2(또는 2') 를 따른다.

## 1. Tailscale

1. Tailscale 을 설치하고 회사 계정으로 로그인한다.
2. 닿는지 확인한다.

   ```powershell
   curl.exe https://cimon.<tailnet>.ts.net/healthz
   ```

   `{"ok":true}` 가 나오면 된다. 안 되면 「문제 해결」.

## 2. 토큰 등록 (Windows)

`claude mcp add --header` 는 토큰을 설정 파일에 평문으로 남긴다. 대신 Claude Code 가 연결할 때마다 실행하는 **`headersHelper`** 로, DPAPI 로 암호화한 파일에서 토큰을 읽는다.

**① 토큰을 암호화해 저장** (PowerShell, 한 번):

```powershell
New-Item -ItemType Directory -Force "$env:USERPROFILE\.cimon" | Out-Null
Read-Host -AsSecureString "cimon token" | ConvertFrom-SecureString |
  Set-Content "$env:USERPROFILE\.cimon\token.dpapi"
```

이 파일은 이 PC 의 이 Windows 사용자만 풀 수 있다.

**② `%USERPROFILE%\.cimon\header.ps1`** — 헤더 JSON 을 출력한다:

```powershell
$secure = Get-Content "$env:USERPROFILE\.cimon\token.dpapi" | ConvertTo-SecureString
$token = [System.Net.NetworkCredential]::new('', $secure).Password
@{ Authorization = "Bearer $token" } | ConvertTo-Json -Compress
```

**③ Claude Code 에 등록** — **명령 프롬프트(cmd)** 에서. Windows PowerShell 5.1 은 인자 속 따옴표를 깨뜨린다. `<me>`·`<tailnet>` 을 바꾼다:

```bat
claude mcp add-json --scope user cimon "{\"type\":\"http\",\"url\":\"https://cimon.<tailnet>.ts.net/mcp\",\"headersHelper\":\"powershell -NoProfile -ExecutionPolicy Bypass -File C:/Users/<me>/.cimon/header.ps1\"}"
```

- `--scope user`: 모든 프로젝트 폴더에서 쓴다. 빼면 지금 폴더에서만 보인다.
- `-ExecutionPolicy Bypass`: Windows 기본 실행 정책은 `.ps1` 실행을 막는다.
- `headersHelper` 는 10초 안에 끝나야 한다.

## 2'. 토큰 등록 (macOS · Linux)

```sh
# macOS — 키체인에 저장(입력을 묻는다)
security add-generic-password -a "$USER" -s cimon -w
# Linux — secret-tool(libsecret)
secret-tool store --label=cimon service cimon
```

`~/.cimon/header.sh`:

```sh
#!/bin/sh
t=$(security find-generic-password -a "$USER" -s cimon -w)   # Linux: t=$(secret-tool lookup service cimon)
printf '{"Authorization":"Bearer %s"}' "$t"
```

```sh
claude mcp add-json --scope user cimon '{"type":"http","url":"https://cimon.<tailnet>.ts.net/mcp","headersHelper":"sh /Users/<me>/.cimon/header.sh"}'
```

Linux 는 `/home/<me>/.cimon/header.sh`.

## 3. 확인

Claude Code 에서 `/mcp` 를 열어 `cimon` 이 연결됐는지 본다. 그다음 「cimon 프로젝트 목록 보여 줘」 → `list_projects` 가 등록된 프로젝트를 돌려주면 끝이다.

## 쓰기

평소 말로 시키면 Claude 가 도구를 고른다.

| 이렇게 말하면 | 도구 |
|---|---|
| 「`<project>` 의 `feature/x` 빌드해서 산출물 받아 줘」 | `build` → `artifacts` |
| 「테스트까지 돌려 줘」 | `run`(기본 build + 기본 test) |
| 「테스트만」 | `test`(필요한 빌드는 따라온다) |
| 「돌리기 전에 뭘 하는지 보여 줘」 | `plan` — 실행하지 않는다 |
| 「왜 실패했어?」 | `job_status` · `job_log`(단계 로그 끝 N 줄, 패턴 검색) |
| 「취소해」 · 「대기열 보여 줘」 | `cancel_job` · `queue` |
| 「이 프로젝트는 어떤 단계·산출물이 있어?」 | `describe_project` |

입력은 `project` 와 선택 항목들이다 — `ref`(브랜치 이름이나 커밋 SHA, 기본은 프로젝트의 `default_ref`), `steps`·`tags`, `artifacts`(받을 산출물 이름), `params`(레시피에 선언된 것만), `clean`(warm 트리 대신 일회 클론).

**기다리는 동안**: `build`·`test`·`run` 은 MCP task 로 돌아 바로 돌아온다. Claude Code 가 끝날 때까지 확인하므로 대화를 이어 가도 된다. 끝나면 판정·요약·산출물 링크가 온다. task 를 모르는 클라이언트는 작업 id(`j000123`)를 받고 `job_status` 로 확인한다.

**판정 읽기**:

| 판정 · 분류 | 뜻 | 할 일 |
|---|---|---|
| `pass` | 돈 단계가 전부 통과하고 요청한 산출물을 다 모았다 | — |
| `fail` · `code` | 컴파일 에러, 테스트 실패, 시간 초과 등 | 내 코드를 본다 |
| `fail` · `request` | 없는 브랜치, 허용 브랜치에서 닿지 않는 커밋 | 요청을 고친다 |
| `fail`·`inconclusive` · `recipe` | 레시피·판정 스크립트 문제 | 레시피 담당에게, 또는 레시피 제안 |
| `inconclusive` · `environment` | 툴체인 미설치, 디스크 부족 | 관리자(`cimon provision`) |
| `inconclusive` · `infra` | fetch·다운로드 실패, 서비스 재시작 | 다시 요청, 반복되면 관리자 |

경고(`warnings`)도 읽는다 — warm 슬롯을 다시 만들었다, 빌드 계정에 남은 프로세스를 정리했다 같은 것이 여기 온다.

## 산출물 받기

- `artifacts` 가 산출물 목록과 **1시간짜리 다운로드 링크**를 준다. 링크 자체가 자격 증명이라 토큰 헤더 없이 tailnet 안 기기에서 받는다. 링크를 공개 채널에 올리지 않는다.
- 끊기면 이어받는다: `curl.exe -C - -o <파일> "<링크>"`(Range 지원).
- 링크가 만료되면(403) `artifacts` 를 다시 부르면 새 링크가 나온다. 보관 기간(기본 7일)이 지나면 410 — 다시 빌드한다.
- 각 산출물에는 sha256 이 있고, 작업마다 어떤 커밋·레시피·툴체인으로 지었는지 `provenance.json` 이 함께 있다.

## 레시피 고치기

빌드 방법(레시피)은 빌드 머신 안 저장소에 있고, 누구나 **제안**으로 고칠 수 있다. `main` 에 넣는 것은 관리자다.

1. 「`<project>` 레시피에 lint 단계 추가해 줘」 → Claude 가 `recipe_show` 로 읽고 `recipe_edit` 로 `recipe/<설명>` 브랜치에 커밋한다 — 제안이 열린다.
2. 그 브랜치로 시험한다: 「`recipe/<설명>` 레시피로 `<project>` run 돌려 줘」(`recipe_ref`).
3. `recipe_propose` 로 제목·설명을 적는다. 관리자가 `recipe_proposal` 이나 리뷰 화면(`web_login` 이 1회용 로그인 링크를 준다)에서 보고 `recipe_promote` 로 반영한다.

시도 실행이 통과해야 반영 조건이 찬다(design §6.5).

## 문제 해결

| 증상 | 확인 |
|---|---|
| `/mcp` 에서 연결 실패 | Tailscale 이 켜져 있나, `curl.exe https://cimon.<tailnet>.ts.net/healthz` |
| healthz 도 안 됨 | `tailscale status` 에 `cimon` 이 보이나. 안 보이면 tailnet 관리자(ACL·그룹) |
| 401 | 토큰이 틀렸거나 꺼졌다 — 관리자에게 `cimon token rotate <이름>` |
| headersHelper 오류 | `powershell -NoProfile -ExecutionPolicy Bypass -File C:/Users/<me>/.cimon/header.ps1` 을 직접 실행해 JSON 한 줄이 나오는지 |
| `queue_full` | 내 대기 작업이 상한(기본 5개)이다 — 기다리거나 `cancel_job` |
| `draining` | 관리자가 업그레이드 중이다 — 잠시 뒤 다시 |
| `ref_not_allowed` · `unknown_project` | 그 프로젝트가 허용한 브랜치·등록된 id 인지 — `describe_project`, `list_projects` |
| 느리다 | `tailscale status` 에 `relay` 가 보이면 중계(DERP) 경로다. 다운로드는 이어받기로 |

## 보안 수칙

- 토큰은 **나의 신원**이다. 모든 호출이 토큰 사용자 이름으로 감사 로그에 남는다.
- 토큰을 Claude 대화·이슈·채팅에 붙여 넣지 않는다.
- PC 를 잃어버렸거나 토큰이 샜으면 바로 관리자에게 — 관리자가 tailnet 에서 기기를 빼고 `cimon token disable <이름>` 한다. 사용자별 토큰이라 다른 사람은 영향이 없다.
