# cimon

> **Cimon says: build.**

cimon(「시몬」)은 Claude Code 가 MCP tasks 로 부르는 원격 빌드·테스트 서비스다. 팀원이 대화 중에 「이 브랜치 빌드해서 받아 줘」 하면, Windows 빌드 머신이 레시피대로 툴체인을 갖추고 빌드·테스트를 돌린 뒤 판정과 산출물 링크를 돌려준다.

이 저장소는 **어떤 프로젝트를 빌드하는지 모른다.** 프로젝트별 레시피는 cimon 이 빌드 머신 안에 갖는 레시피 저장소(`cimon.toml [recipes].store`, git)에 두고, MCP 도구로 고친다.

| 문서 | 내용 |
|---|---|
| [docs/remote-use.md](docs/remote-use.md) | **외부에서 cimon 쓰기** — 팀원 PC 준비(Tailscale·토큰·Claude Code 등록), 빌드·산출물 받기, 레시피 제안, 문제 해결 |
| [docs/setup.md](docs/setup.md) | 빌드 머신 셋업 절차(관리자) |
| [docs/design.md](docs/design.md) | 서비스 구조 · MCP 인터페이스 · 작업 모델 · 보안 · 운영 · 구현 순서 |
| [docs/recipe-spec.md](docs/recipe-spec.md) | 레시피 파일 형식 |
| [docs/review-ui.md](docs/review-ui.md) | 레시피 리뷰 웹 화면 |
| [examples/recipes/](examples/recipes/) | 형식 예시(레시피 검사기의 시험 입력) |

## 쓰기(팀원)

회사 tailnet 에 붙은 PC 라면 사무실 밖에서도 쓴다. 관리자에게 초대 코드(`cimon invite <이름>`)를 받고, Tailscale 에 로그인한 뒤 한 줄을 실행해 코드를 입력하면 연결이 끝난다(Claude Code v2.1.304 이상).

```powershell
irm https://<서버>/connect.ps1 | iex          # Windows
curl -fsSL https://<서버>/connect.sh | sh     # macOS · Linux
```

토큰 저장·등록이 무엇을 하는지, 손으로 하는 법, 사용 예는 **[docs/remote-use.md](docs/remote-use.md)**.

## 처음 올리기(빌드 머신, 관리자)

명령 단위 절차는 [docs/setup.md](docs/setup.md). 요약(관리자 PowerShell, `cimon.exe` 는 `C:\Program Files\cimon\`):

```powershell
cimon setup --dry-run                                   # 무엇을 하는지 먼저 본다 — 설정은 D:\cimon\config\cimon.toml
cimon setup
cimon github-token                                      # 토큰을 붙여 넣고 Enter(명령줄·기록에 남기지 않는다)
cimon token create <이름> --admin                        # 토큰은 한 번만 보인다
cimon recipe import --init <레시피 폴더> --author <이름>
cimon provision <project>                               # auto_approve 밖의 툴체인
cimon doctor --network
sc.exe start cimon                                      # PowerShell 의 sc 는 Set-Content 다
```

그 전에 Tailscale(`tailscale serve --bg --https=443 http://127.0.0.1:8770`, Funnel 금지)과 `cimon.toml` 의 `[bootstrap]` 버전 고정이 필요하다 — setup.md 1·3단계.

## 빌드 · 시험

Rust 1.94 이상, Windows(MSVC), git 2.40 이상.

```
cargo build --release          # target\release\cimon.exe 하나
cargo test --workspace         # 단위 + 통합(서버를 프로세스 안에서 띄우고 rmcp 클라이언트로 부른다)
```

통합 시험은 로컬 git 저장소를 `https://example.invalid/<이름>.git` 으로 보이게 하고(git `insteadOf`), 단일 계정 모드(`[accounts].build = ""`)로 돈다.

## 검증 상태

- ✅ M0~M9 의 완료 기준을 단위·통합 시험으로 확인 — 로컬 Windows 개발 PC, 단일 계정 모드.
- ✅ 구현 뒤 객관 리뷰(작업 수명주기·레시피 저장소·Windows 러너·보안)와 셋업 절차 점검에서 나온 결함을 고치고 회귀 시험을 더했다 — 작업 트리 git 을 빌드 계정으로, 계정 사이 git 소유자 검사, `[bootstrap]` 버전 고정 포함. `cargo test --workspace` 149개.
- ✅ 2026-10-26 실제 Windows 11 PC 한 대(Tailscale 을 사용자 계정으로 쓰는 개인 tailnet)에 docs/setup.md 대로 올렸다 — `cimon setup`, 빌드 계정 모드(로그온·프로필 하이브·`CreateProcessAsUserW`·스윕·작업 트리 git), Windows 서비스, tailnet 너머 Claude Code 호출, 설치기(archive·github-release·uv·rustup·script, `--from`), 실제 프로젝트 빌드·테스트(통과, 산출물 내려받기와 sha256 확인), 레시피 제안·시도 실행·반영. 올리며 나온 결함은 고치고 setup.md 「실제 머신에서 확인한 것」 에 적었다.
- ⚠️ 아직 돌려 보지 않은 것: 판정·검사 스크립트, fixture, 장시간 작업의 취소·서비스 재시작 중 중단, 리뷰 화면, 외부 백업 사본.
