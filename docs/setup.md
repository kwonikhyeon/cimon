# 빌드 머신 셋업

관리자가 Windows 빌드 머신 한 대에 cimon 을 처음 올리는 절차다. 설계 근거는 [design.md](design.md) §5·§15·§16, 팀원 PC 쪽은 [remote-use.md](remote-use.md).

명령은 모두 **관리자 PowerShell** 에서 실행한다. 아래에서 `$c` 는 cimon 실행 파일이다.

```powershell
$c = "C:\Program Files\cimon\cimon.exe"
```

## 0. 머신 준비

| 항목 | 설정 |
|---|---|
| 사양 | 16스레드 이상 · RAM 64 GB · NVMe 2 TB |
| 디스크 | `D:` 를 Dev Drive(ReFS)로 |
| 경로 길이 | `LongPathsEnabled=1` |
| Defender | Dev Drive 성능 모드, 또는 `D:\cimon` 예외 |
| Smart App Control | 끈다 — 빌드한 실행 파일의 실행을 막은 사례가 있다 |
| Windows Update | 사용 시간대 설정 — 빌드 도중 재부팅 금지 |
| 전원 | 절전 안 함 |

**base 툴체인** — 레시피 저장소 카탈로그(`toolchains.toml`)의 `tier = "base"` 항목이다. 관리자가 미리 설치하고 cimon 은 검사만 한다(design §10.2). 흔히 Visual Studio Build Tools(C++ 워크로드 + Windows SDK), Git for Windows 등이다. 빠진 것은 9단계의 `cimon doctor` 가 프로젝트별로 알려 준다.

cimon 자신이 쓰는 것은 **Git for Windows 2.40 이상**(시스템 PATH)이다.

```powershell
git config --system core.longpaths true
```

## 1. Tailscale (design §15.3)

1. 설치하고 관리자 계정으로 `tailscale up --unattended=true --advertise-tags=tag:cimon`.
2. 관리 콘솔에서
   - 기기 이름을 `cimon` 으로, 이 기기의 **키 만료를 끈다**(기본 180일마다 재인증).
   - MagicDNS·HTTPS 인증서를 켠다.
   - ACL 을 design §15.3 초안대로: 기본 「모두 → 모두」 규칙을 지우고, `tag:cimon` 에서 나가는 grant 는 두지 않는다.
3. `tailscale serve --bg --https=443 http://127.0.0.1:8770` — 재부팅 뒤에도 유지된다.
4. **Funnel 은 켜지 않는다.** 서비스는 시작할 때 `tailscale serve status --json` 으로 Funnel 이 꺼졌는지 확인하고, 켜져 있거나 확인하지 못하면 시작을 거부한다. 그래서 이 단계를 서비스 시작보다 먼저 끝낸다.

## 2. cimon.exe 배치

`cargo build --release` 로 만든 `target\release\cimon.exe` 를 **`C:\Program Files\cimon\`** 에 둔다.

- `D:\cimon` 아래에 두지 않는다. setup 이 `D:\cimon` 의 권한 상속을 끊어 빌드 계정은 정해진 폴더만 읽는데, 빌드 단계는 빌드 계정으로 `cimon.exe __exec` 를 거쳐 돈다 — 빌드 계정이 실행 파일을 읽을 수 있어야 한다.
- setup 은 **자기를 실행한 경로**를 서비스 경로로 등록한다. 최종 위치에 둔 파일로 실행한다.

## 3. 설정 파일

`D:\cimon\config\cimon.toml` 을 먼저 쓴다(setup 이 읽는다). 위치를 바꾸려면 모든 명령에 `--config <경로>` 나 환경 변수 `CIMON_CONFIG`. 여기 없는 키의 기본값과 설명은 design §14.

```toml
[server]
public_url = "https://cimon.<tailnet>.ts.net"
allowed_hosts = ["cimon.<tailnet>.ts.net"]   # 없으면 tailscale serve 를 거친 요청이 Host 검사에서 거절된다

[paths]
root = 'D:\cimon'

[recipes]
allowed_repo_prefixes = ["https://github.com/<org>/"]

[base]                                       # 카탈로그 base 항목의 설치 위치
git = 'C:\Program Files\Git'

[bootstrap]                                  # 필수 — setup 이 이 버전을 체크섬과 함께 받는다
rustup = "1.29.2"                            # https://static.rust-lang.org/rustup/release-stable.toml
uv = "0.13.5"                                # https://github.com/astral-sh/uv/releases

[toolchains]
auto_approve = ["rust", "node", "python", "cmake", "task"]

[backup]
offsite = 'E:\cimon-backup'                  # 빌드 머신 밖 사본. 안 두려면 "none"(모든 recipe_* 결과에 경고)
```

`[backup].offsite` 를 네트워크 공유로 두면 주의한다 — 서비스 계정(`NT SERVICE\cimon`)은 네트워크에 **컴퓨터 계정**으로 나가므로, 도메인에 가입하지 않은 머신에서는 공유에 인증되지 않는다. 처음에는 다른 로컬 디스크가 안전하다.

## 4. `cimon setup`

```powershell
& $c setup --dry-run     # 할 일 목록만
& $c setup
```

하는 일 — 실패한 단계는 `✗` 로 표시되고, 하나라도 실패하면 종료 코드가 0 이 아니다. 다시 돌려도 된다.

1. `D:\cimon` 아래 디렉터리
2. Windows 서비스 `cimon` 등록(`NT SERVICE\cimon`, 자동 시작, 실패 시 재시작)
3. 산출물 링크 서명 키 `config\link.key`
4. 빌드 계정 `cimon-build` — 무작위 비밀번호, 만료 없음, `config\build-account.dpapi` 에 DPAPI(머신 범위)로 저장
5. 권한 — 빌드 계정: 「일괄 작업으로 로그온」. 서비스 계정: 「서비스로 로그온」·프로세스 토큰 바꾸기·메모리 할당량 조정·백업·복원(`LoadUserProfile` 이 요구한다, design §9.2)
6. ACL(design §5) — 서비스는 전부, 빌드 계정은 `ws`·`caches`·`homes`·`tmp` 쓰기, `mirrors`·`toolchains`·`recipes`·`artifacts` 읽기, 나머지는 접근 불가
7. `[bootstrap]` 버전의 rustup·uv 를 `toolchains\_bootstrap\` 에(체크섬 확인)

## 5. 빌드 계정 아웃바운드 제한 (수동)

setup 이 자동화하지 않는다. Windows 방화벽(또는 WFP)으로 `cimon-build` 의 아웃바운드를 GitHub·패키지 레지스트리·사내 아티팩트 저장소로만 제한한다. 허용할 주소는 레시피 저장소의 프로젝트 노트. 관리자 RDP 는 `100.64.0.0/10`(tailnet)에서만 받는다.

## 6. GitHub 토큰 · 첫 관리자

```powershell
& $c github-token                    # 토큰을 붙여 넣고 Enter — config\github-token.dpapi 에 DPAPI 로
& $c token create <이름> --admin      # 사용자 토큰은 이때 한 번만 보인다
```

- GitHub 토큰은 등록할 저장소만 읽는 **fine-grained 토큰**(Contents·Metadata 읽기)이다. 서비스가 fetch 할 때만 쓰고, 빌드 계정에는 넘기지 않는다.
- `echo <토큰> | cimon github-token` 처럼 넘기지 않는다 — PowerShell 명령 기록에 남는다.

## 7. 레시피 들여오기

사내 레시피 폴더(처음 내용)를 빌드 머신으로 옮긴 뒤:

```powershell
& $c recipe check <레시피 폴더>                                   # 정적 검사만
& $c recipe import --init <레시피 폴더> --author <이름>
```

- 저장소와 `main` 을 한 번 만든다. 정적 검사를 통과해야 하고, `main` 이 이미 있으면 거절한다. 끝나면 바로 번들 백업을 만든다.
- 이후 레시피는 MCP 도구(`recipe_edit` → 제안 → 관리자 `recipe_promote`)로만 고친다(design §6.4).

## 8. 툴체인 미리 설치

`auto_approve` 에 없는 종류는 접수 단계에서 설치되지 않는다 — 그 도구가 필요한 작업은 `inconclusive`/`environment` 로 끝난다. 프로젝트마다 미리 설치한다.

```powershell
& $c provision <project>                         # 그 프로젝트의 툴체인 전부
& $c provision --kind <종류> --version <버전>      # 하나만
```

## 9. 점검

```powershell
& $c doctor --network
```

`FAIL` 이 하나라도 있으면 종료 코드가 0 이 아니다. 디렉터리, 작업·레시피 저장소(`git fsck`, `main` 정적 검사), 프로젝트별 base 툴체인, GitHub 접근(`--network`), 백업, 디스크, Funnel 꺼짐, 빌드 계정 로그온, 서비스 등록을 본다.

## 10. 서비스 시작

```powershell
sc.exe start cimon
```

- PowerShell 에서 `sc` 는 `Set-Content` 의 별칭이다 — 반드시 `sc.exe`.
- tailnet 에 붙은 다른 기기에서 `curl.exe https://cimon.<tailnet>.ts.net/healthz` → `{"ok":true}`. 사용자 토큰(`-H "Authorization: Bearer <토큰>"`)을 붙이면 큐·디스크·레시피 커밋·백업 시각·경고까지 나온다.
- 서비스 로그: `D:\cimon\logs\cimon\`.

## 11. 팀원 등록

팀원마다 tailnet 의 `group:cimon-users` 에 넣고, `& $c token create <이름>` 으로 토큰을 발급해 전달한다(메신저 평문 금지). 팀원이 할 일은 [remote-use.md](remote-use.md).

사용자 관리: `& $c token disable <이름>` · `& $c token rotate <이름>` · `& $c user role <이름> admin|member` · `& $c token list`.

## 12. 첫 빌드

Claude Code 에서 프로젝트 하나를 `build` 로 돌려 판정과 산출물 다운로드까지 확인한다. 아래 「확인할 것」 을 처음 실제로 지나가는 실행이다.

## 업그레이드 (design §16.6)

```powershell
& "<새 cimon.exe>" recipe check --main   # 새 검사기로 main 이 통과해야 한다
& $c drain                               # 새 작업 거절, 실행 중 작업이 끝날 때까지 기다린다
sc.exe stop cimon
# C:\Program Files\cimon\cimon.exe 교체
& $c setup                               # 권한·ACL 을 새 버전 기준으로 맞춘다(다시 돌려도 된다)
sc.exe start cimon                       # 시작할 때 drain 표시를 지운다
```

## 실제 머신에서 아직 확인하지 않은 것

개발 PC 의 단일 계정 모드 시험만 거쳤다(README 「검증 상태」). 처음 올릴 때 특히 볼 것:

- 빌드 계정 모드 — 서비스 계정의 `LoadUserProfile`(백업·복원 권한), `CreateProcessAsUserW`, 빈 `lpDesktop`, 작업 끝 스윕, 작업 트리 git 을 빌드 계정으로 돌리기
- `cimon setup` 의 실제 실행 — 계정 생성, LSA 권한, ACL, 서비스 등록
- 서비스 계정이 `tailscale serve status --json` 을 실행할 수 있는지 — 못 하면 서비스가 시작을 거부한다
- rustup·uv·script 설치기의 실제 다운로드
