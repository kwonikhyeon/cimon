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

base 도구가 **관리자 계정의 사용자 폴더**에 깔려 있으면(Android Studio 의 SDK 는 `%LOCALAPPDATA%\Android\Sdk`, 사용자용 Inno Setup 은 `%LOCALAPPDATA%\Programs\Inno Setup 6`) 빌드 계정이 쓸 수 없다. 그 폴더에만 읽기 권한을 줘도 모자란다 — 도구가 실제 경로를 확인하며 상위 폴더(`C:\Users\<관리자>`·`AppData`…)를 하나씩 읽다가 거절된다(javac 의 `AccessDeniedException … toRealPath`, 실제 머신에서 확인). 시스템 위치에 따로 두고 `[base]` 를 그쪽으로 한다. `doctor` 는 관리자로 돌아서 이 문제를 잡지 못한다.

```powershell
# 예: Android SDK — 필요한 패키지만 든 사용자 SDK 를 통째로(라이선스 파일 포함) 복사
New-Item -ItemType Directory -Force C:\Android\Sdk | Out-Null
icacls C:\Android /inheritance:r /grant:r "*S-1-5-32-544:(OI)(CI)F" "*S-1-5-18:(OI)(CI)F" "*S-1-5-32-545:(OI)(CI)RX"   # 관리자·SYSTEM 만 쓰기 — 빌드가 SDK 를 바꾸지 못하게
robocopy "$env:LOCALAPPDATA\Android\Sdk" C:\Android\Sdk /E /XD .temp
```

`C:\` 바로 아래 새 폴더는 기본으로 인증된 사용자 누구나 수정할 수 있다 — 위처럼 권한을 잡지 않으면 빌드 단계가 base 도구를 바꿔 다른 작업에 영향을 줄 수 있다.

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
4. **Funnel 은 켜지 않는다.** 서비스는 시작할 때 `tailscale serve status --json` 으로 Funnel 이 꺼졌는지 확인하고, 켜져 있으면 시작을 거부한다. 그래서 이 단계를 서비스 시작보다 먼저 끝낸다.
5. **Windows 에서 Tailscale 이 사용자 계정으로 로그인돼 있으면** Tailscale 은 그 사용자에게만 답한다 — 서비스 계정의 조회는 `401 Unauthorized: Tailscale already in use by <사용자>` 로 거절되고, 서비스는 Funnel 을 확인할 수 없어 시작하지 않는다. 그런 머신에서는 `cimon.toml` 에 `[tailscale] allow_unverified_funnel = true` 를 두고 대신 이렇게 보장한다.
   - tailnet 정책(Access controls)에 이 기기용 `funnel` 노드 속성(`"nodeAttrs"` 의 `"attr": ["funnel"]`)을 두지 않는다 — 그러면 Funnel 자체를 켤 수 없다. 관리 콘솔 JSON 편집기 오른쪽에 「Add Funnel to policy」 가 보이면 정책에 없는 것이다(누르지 않는다).
   - `cimon doctor` 를 Tailscale 이 로그인된 그 사용자(관리자 PowerShell)로 돌리면 Funnel 을 실제로 확인한다.
   - 서비스는 확인하지 못했다는 경고를 로그와 `/healthz` 에 계속 싣는다. 켜진 것이 보이면 이 설정과 상관없이 거부한다.

## 2. cimon.exe 배치

`cargo build --release` 로 만든 `target\release\cimon.exe` 를 **`C:\Program Files\cimon\`** 에 둔다. 알림 영역 앱(docs/tray.md)을 쓰려면 `target\release\cimon-tray.exe` 도 같은 폴더에 둔다 — 트레이는 옆의 `cimon.exe` 만 부르고, 그 폴더에서 돌 때만 아이콘 위치를 Windows 가 기억한다.

- `D:\cimon` 아래에 두지 않는다. setup 이 `D:\cimon` 의 권한 상속을 끊어 빌드 계정은 정해진 폴더만 읽는데, 빌드 단계는 빌드 계정으로 `cimon.exe __exec` 를 거쳐 돈다 — 빌드 계정이 실행 파일을 읽을 수 있어야 한다.
- setup 은 **자기를 실행한 경로**를 서비스 경로로 등록한다. 최종 위치에 둔 파일로 실행한다.

## 3. 설정 파일

`D:\cimon\config\cimon.toml` 을 먼저 쓴다(setup 이 읽는다). 여기 없는 키의 기본값과 설명은 design §14.

다른 위치(예: `D:` 가 없어 `C:\cimon`)에 두면 setup 을 `--config <경로>` 로 **한 번** 돌린다 — 서비스 명령줄에 그 경로가 박히고, 그 뒤로 CLI 는 `--config` 없이도 설치된 서비스의 명령줄에서 경로를 읽는다(찾는 순서: `--config` → `$CIMON_CONFIG` → 서비스의 `--config` → `D:\cimon\config\cimon.toml`).

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
5. 권한 — 빌드 계정: 「일괄 작업으로 로그온」. 서비스 계정: 「서비스로 로그온」·프로세스 토큰 바꾸기·메모리 할당량 조정·백업·복원(빌드 계정 프로필 하이브를 올리는 데 쓴다, design §9.2)
6. 빌드 계정 프로필 — 사용자 프로필 서비스는 관리자의 호출에만 프로필을 만들어 준다. setup(관리자)이 한 번 만들어 두고, 서비스는 그 하이브를 올려 쓴다
7. ACL(design §5) — 서비스는 전부, 빌드 계정은 `ws`·`caches`·`homes`·`tmp` 쓰기, `mirrors`·`toolchains`·`recipes`·`artifacts` 읽기, 루트 폴더 자체는 목록만(위로 올라가며 폴더를 읽는 빌드 도구 때문), 나머지는 접근 불가
8. `[bootstrap]` 버전의 rustup·uv 를 `toolchains\_bootstrap\` 에(체크섬 확인)

## 5. 빌드 계정 아웃바운드 제한 (수동)

setup 이 자동화하지 않는다. Windows 방화벽(또는 WFP)으로 `cimon-build` 의 아웃바운드를 GitHub·패키지 레지스트리·사내 아티팩트 저장소로만 제한한다. 허용할 주소는 레시피 저장소의 프로젝트 노트. 관리자 RDP 는 `100.64.0.0/10`(tailnet)에서만 받는다.

## 6. GitHub 토큰 · 첫 관리자

```powershell
& $c github-token                    # 입력이 보이지 않는 프롬프트에 토큰을 붙여 넣고 Enter — config\github-token.dpapi 에 DPAPI 로
& $c token create <이름> --admin      # 사용자 토큰은 이때 한 번만 보인다
```

- GitHub 토큰은 등록할 저장소만 읽는 **fine-grained 토큰**(Contents·Metadata 읽기)이 낫다. classic 토큰·`gh` 로그인 토큰도 되지만 `repo` 범위라 접근할 수 있는 모든 비공개 저장소에 쓰기까지 된다. 서비스가 fetch 할 때만 쓰고, 빌드 계정에는 넘기지 않는다.
- 토큰을 받는 명령(`github-token`·`build-git-token`·`build-secret`)은 콘솔에서 그냥 실행하면 **입력이 보이지 않는 프롬프트**로 묻는다 — 화면·명령 기록에 남지 않는다. 파이프로도 받는다: `gh auth token | & $c github-token`(PowerShell 이 앞에 붙이는 BOM 은 cimon 이 뗀다). `echo <토큰> | …` 처럼 명령줄에 토큰을 쓰지 않는다 — PowerShell 명령 기록에 남는다.
- `doctor --network` 의 `GitHub access` 가 모두 OK 여야 한다.

빌드 중에 **다른 저장소를 git 으로 받는** 프로젝트(cargo `git` 의존성 등 — 레시피 저장소의 프로젝트 노트에 적혀 있다)는 그 저장소가 **cimon 에 등록된 프로젝트면 아무것도 할 필요가 없다** — 빌드 단계는 cimon 의 mirror 에서 받는다(design §11.6). 등록되지 않은 비공개 저장소일 때만 빌드 계정에 그 저장소 하나를 읽는 토큰을 둔다. 모든 빌드 단계가 볼 수 있으니 fine-grained 로 그 저장소의 Contents 읽기만 준다.

```powershell
& $c build-git-token https://github.com/<org>/<repo>.git   # 토큰을 묻는다. URL 은 빌드가 받는 주소 그대로(ssh 주소는 https 로 바뀐 뒤의 모양)
& $c build-git-token                                        # 등록된 URL 목록
```

git 이 아닌 도구가 **자기 자격 증명**을 요구하는 프로젝트(사내 Conan·npm 레지스트리 등 — 레시피가 `${secret.<name>}` 으로 쓴다)는 그 프로젝트에 **빌드 비밀**을 둔다. `plan` 의 `secrets`, `doctor` 의 `build secret` 이 무엇이 필요한지 보인다. 그 변수를 받는 단계가 모두 읽을 수 있으니 **읽기 전용** 토큰(예: 레지스트리를 읽기만 하는 전용 사용자의 토큰)을 쓴다.

```powershell
& $c build-secret <project> <name>                  # 값을 묻는다(시도 레시피 작업은 관리자가 요청할 때만 받는다). 예: build-secret example-app artifactory-conan-token
"cimon-build" | & $c build-secret <project> <name>  # 비밀이 아닌 값(사용자 이름 등)은 파이프로
& $c build-secret                                   # 저장된 비밀 목록(값은 보이지 않는다)
& $c build-secret <project> <name> --remove
```

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

팀원마다 tailnet 의 `group:cimon-users` 에 넣고 **초대 코드**를 만든다 — 기기마다 이름 하나(잃어버린 기기만 끌 수 있게).

```powershell
& $c invite <이름>             # 10분·한 번 쓰는 코드와, 팀원이 실행할 한 줄을 출력한다(--admin, --minutes)
```

팀원은 그 한 줄(`irm …/connect.ps1 | iex` 또는 `curl -fsSL …/connect.sh | sh`)을 실행하고 코드를 입력하면 끝이다 — 토큰은 그 기기에서 만들어져 바로 OS 보관소에 들어가므로 사람 손을 거치지 않는다([remote-use.md](remote-use.md) 「빠른 연결」). 코드 대신 토큰을 직접 주려면 `& $c token create <이름>`(한 번만 보인다, 메신저 평문 금지).

사용자 관리: `& $c token disable <이름>` · `& $c token rotate <이름>` · `& $c user role <이름> admin|member` · `& $c token list`.

## 12. 첫 빌드

Claude Code 에서 프로젝트 하나를 `build` 로 돌려 판정과 산출물 다운로드까지 확인한다. 아래 「확인할 것」 을 처음 실제로 지나가는 실행이다.

## 13. 알림 영역 앱 (선택, docs/tray.md)

```powershell
& $c setup --service-acl                 # 서비스 DACL — 로그인한 사용자는 조회만(트레이의 「작업 끝나면 중지」 전에 필요)
& $c setup --operator $env:USERNAME      # (권장) 이 사용자는 UAC 없이 켜고 끈다 — 다음 로그인부터
& "C:\Program Files\cimon\cimon-tray.exe" # 처음 실행 때 로그인할 때 시작할지 묻는다
```

아이콘은 숨겨진 아이콘 칸(^)에 들어간다. 작업 표시줄로 끌어다 놓거나 메뉴의 「작업 표시줄에 항상 보이게…」 로 꺼낸다. `cimon ctl status` 는 관리자가 아니어도 서비스 상태와 DACL 의 문제를 보여 준다.

## 업그레이드 (design §16.6)

```powershell
& $c upgrade --from "<새 cimon.exe>"     # 확인 → recipe check --main → 작업 끝나면 중지 → 교체(옆의 cimon-tray.exe 도) → 시작 → 새 build 확인, 안 되면 되돌림
& $c setup                               # 권한·ACL 을 새 버전 기준으로 맞춘다(다시 돌려도 된다)
```

트레이의 「관리자 > 새 버전 올리기…」 가 같은 명령을 UAC 창에서 돌린다. 실행 중인 작업을 기다리지 않으려면 `--now`. 손으로 하려면: `recipe check --main` → `drain` → `sc.exe stop cimon` → 교체 → `sc.exe start cimon`.

## 실제 머신에서 확인한 것

2026-10-26, Tailscale 을 사용자 계정으로 쓰는 Windows 11 PC 한 대(`C:\cimon`, 개인 tailnet)에 이 절차대로 올렸다.

- 확인함: setup 전 단계(계정·권한·서비스 등록·ACL·bootstrap), 빌드 계정 로그온, 서비스 기동, tailnet 에서 `/healthz`·토큰 거절(401), Claude Code 로 `list_projects`, archive·github-release·uv·rustup 설치기, script 설치기를 빌드 계정으로 실행, 작업 하나를 빌드 계정으로 끝까지(시작 스윕 → 작업 트리 git → setup·build 단계 → 결과·경고).
- 올리며 고친 것: 서비스 시작 실패가 로그에 남지 않던 것, 서비스 계정의 Tailscale 조회 거절(위 1단계 5), `LoadUserProfile` 거절(프로필 하이브를 직접 올림), 루트 폴더 목록 권한, PowerShell 파이프가 붙인 토큰 앞 BOM, provision 이 script 설치기의 mirror 를 받지 않던 것.
- 아직: 판정·검사 스크립트, fixture, `[account_env]` 를 쓰는 프로젝트의 실행, 장시간 작업의 취소·서비스 재시작 중 중단.
