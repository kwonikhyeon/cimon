# cimon 설계

> **Cimon says: build.**

- 이름: **cimon**(「시몬」 으로 읽는다). CI + mon — zenmon(zenoh + monitor)의 형제 이름이다. 「Simon says」(시키는 대로 하는 놀이)처럼, 사람이 Claude 를 통해 시키면 빌드하고 결과를 가져온다. 같은 이름의 PLC·SCADA 브랜드(대문자 CIMON)와 헷갈리지 않도록 소문자로 쓴다.
- 상태: 2026-10-26 — M0~M9 구현됨. 관리자 권한·실제 빌드 머신이 필요한 부분(빌드 계정 모드, `cimon setup` 실제 실행, Windows 서비스 기동, Tailscale)은 아직 그 환경에서 돌려 보지 않았다 — README 「검증 상태」
- 「확인 필요」 는 아직 실측·실행으로 확인하지 않은 가정이다.

| 문서 | 내용 |
|---|---|
| **design.md**(이 문서) | 서비스 구조 · MCP 인터페이스 · 작업 모델 · 실행 · 저장 · 보안 · 운영 |
| [recipe-spec.md](recipe-spec.md) | 레시피 파일 형식(정본) |
| [review-ui.md](review-ui.md) | 레시피 리뷰 웹 화면 — 구현은 마지막(M9) |
| [setup.md](setup.md) | 빌드 머신 셋업 절차(관리자) |
| [remote-use.md](remote-use.md) | 외부에서 cimon 쓰기(팀원) |
| [`examples/recipes/`](../examples/recipes/) | 형식 예시이자 레시피 검사기의 시험 입력 |

**이 저장소는 어떤 프로젝트를 빌드하는지 모른다.** 실제 프로젝트의 레시피·툴체인 카탈로그·프로젝트별 사실과 결정은 **cimon 이 빌드 머신 안에 직접 갖는 레시피 저장소**(`D:\cimon\recipes.git`)에 있다. GitHub 등 외부에 올리지 않는다. 사람은 MCP 도구로 고친다(§6.4).

---

## 1. 무엇을 만드나

cimon 은 팀원의 Claude Code 세션이 MCP 로 부르는 **원격 빌드·테스트 서비스**다.

- 요청: 「이 프로젝트의 이 ref 를 빌드해라 / 테스트해라 / 둘 다 해라」.
- 응답: MCP tasks 로 **작업 ID 를 즉시** 돌려주고, 작업은 뒤에서 돈다. 끝나면 판정·요약·산출물 다운로드 주소를 돌려준다.
- 프로젝트마다 필요한 툴체인은 **레시피에 적힌 대로 서비스가 직접 갖춘다.** 관리자 권한이 드는 것만 사람이 미리 깐다.
- **새 프로젝트 추가와 테스트 수정은 레시피(데이터) 변경으로 끝난다.** 서비스 코드를 고치거나 다시 배포하지 않는다.

### 1.1 1단계 범위

| 포함 | 제외 — 이유 |
|---|---|
| Windows 빌드 머신 1대, 러너 1개(작업 하나씩) | Linux·macOS·ARM, 동시 실행 — §17 |
| 레시피 저장소에 등록된 프로젝트(1단계는 3개) | GPU·제조사 SDK 가 필요한 프로젝트 |
| 빌드 · 테스트 · 패키징 · 시뮬레이터 위 시나리오 시험 | 실장비 시험(HIL) |
| 서명 없는 테스트 산출물 다운로드 | 서명·릴리스 발행 — 서명 키를 이 머신에 두지 않는다(§15) |
| 화면이 필요 없는 테스트 | 화면에 그리는 테스트 — 서비스는 데스크톱 없는 세션 0 에서 돈다. 레시피가 `needs = ["display"]` 로 표시하면 **제외되고 제외 사실이 결과에 보인다** |
| 레시피 편집·제안·반영(MCP), 리뷰 웹 화면(마지막) | — |
| 팀 tailnet(Tailscale) 안에서만 | 공개 인터넷 노출(Tailscale Funnel 포함) |

---

## 2. 설계 원칙

1. **서비스는 프로젝트를 모른다.** 프로젝트에 관한 모든 것은 레시피 저장소의 데이터다. 서비스 코드에 특정 프로젝트·특정 도구를 위한 분기를 두지 않는다.
2. **프로젝트의 빌드를 다시 구현하지 않는다.** 프로젝트 자신의 진입점(빌드 CLI, `task`, `gradlew`, 스크립트 …)을 부른다.
3. **선언한 것만 쓴다.** 레시피에 없는 툴체인으로 대신 짓지 않는다. 툴체인 버전은 정확히 고정한다. 서비스 프로세스의 `PATH` 를 자식에게 물려주지 않는다.
4. **실패는 실패로 보인다.** 판정은 통과·실패·판정 불가 셋이다. 건너뛴 단계, 빠진 산출물, 0개 실행된 테스트, 아무것도 돌지 않은 작업은 통과가 아니다. 제외한 것과 경고는 결과에 적는다. 실패가 코드 탓인지 환경 탓인지 가른다.
5. **결과는 서버가 갖는다.** Claude Code 는 세션을 끝내면 task 를 더 따라가지 않는다. 작업 ID 만 있으면 다른 세션·다른 날에도 결과와 산출물을 찾을 수 있어야 한다.
6. **확장은 데이터로.** 새 프로젝트·새 테스트·새 도구 종류는 레시피와 카탈로그로 표현한다. 코드가 바뀌는 것은 새 「방식」(판정기·설치 방식·러너 능력)이 필요할 때뿐이다.
7. **권한과 설치에 닿는 것은 레시피 저장소 `main` 에서만.** 저장소 주소·허용 ref·형제 저장소(`project.toml`)와 **툴체인 카탈로그의 설치 항목**은 `main` 의 것을 쓴다. 빌드하는 브랜치도, 시도 실행 브랜치도 권한을 넓히거나 설치되는 바이너리를 바꾸지 못한다.
8. **관리자 권한이 드는 일은 사람이 한다.** MCP 호출 하나로 시스템 전역 설치가 일어나지 않는다.
9. **빌드는 임의 코드 실행이다.** 빌드·설치 스크립트·받은 바이너리의 실행은 서비스와 다른 계정에서 돌고, 그 계정은 서비스의 상태를 건드리지 못한다.

---

## 3. 세 장면

예시 프로젝트 `example-app`(Rust 백엔드 + Gradle 로 만드는 Android 앱)으로 설명한다.

**장면 1 — 빌드하고 테스트하고 산출물 받기**

> 「example-app 의 Android APK 빌드해서 받고, 공유 모듈 테스트도 돌려 줘」

1. Claude → `run(project="example-app", steps=["build-apk", "test-shared"], artifacts=["app-debug.apk"])`
2. cimon: 입력을 레시피로 검증(모르는 단계·파라미터면 여기서 거절) → 작업을 DB 에 커밋 → **바로** task 응답.
3. 접수 단계(러너 밖): fetch → 커밋 고정 → 접수 검사 → 계획 → 툴체인 확인. `/tasks` 에 「접수: git fetch」 → 「대기열 1번째」.
4. 실행 단계(러너): 「build-apk (1/2) · 3분 10초」 → 「test-shared (2/2)」.
5. 끝: 판정 `pass`, 산출물 `app-debug.apk`(sha256, 서명 인증서 지문, 다운로드 주소).
6. Claude 가 `Invoke-WebRequest` 로 받고 sha256 을 대조한다.

**장면 2 — 테스트 고치기**

> 「example-app 에 공유 모듈 desktopTest 만 도는 테스트를 추가하고 돌려 봐」

1. Claude → `recipe_show(ref="main", path="projects/example-app/recipe.toml")` 로 지금 레시피와 커밋을 읽는다.
2. Claude → `recipe_edit(branch="recipe/add-desktop-test", base=<읽은 커밋>, message="desktopTest 단계 추가", changes=[{path, content}])`. cimon 은 **저장 전에 정적 검사**를 돌리고, 통과하면 커밋하고 **제안**을 연다(작성자 = 토큰 사용자). 깨진 레시피는 저장되지 않는다.
3. `test(project="example-app", recipe_ref="recipe/add-desktop-test", steps=["test-shared-desktop"])` → `trial: true` 로 돈다.
4. 관리자: `recipe_proposal(proposal)` 로 의미 차이·반영 조건을 보고 `recipe_promote(proposal, head=<본 커밋>)` — `main` 에 반영, 다음 작업부터 모두에게. 서비스 재시작 없음.

**장면 3 — 새 프로젝트**

1. 관리자가 GitHub 토큰에 저장소를 추가.
2. Claude 가 프로젝트 저장소를 읽어 초안을 쓰고, `recipe_edit` 한 번으로 `project.toml`·`recipe.toml`·노트를 `recipe/add-<id>` 에 만든다 → `plan` → `build`·`test` 로 시도 → 관리자가 반영.

레시피를 고치는 자세한 절차는 레시피 저장소 안의 안내서(`docs/recipe-guide.md`)에 있다.

---

## 4. 구성 요소

```
 Team PC (office or outside)          Build machine (Windows, Tailscale tag:cimon)
 +--------------------+               +------------------------------------------------+
 | claude             |   tailnet     | tailscaled                                     |
 |   /tasks           |  (WireGuard)  |   serve  https://cimon.<tailnet>.ts.net:443    |
 |   Invoke-WebReq.   | ------------> |          -> http://127.0.0.1:8770              |
 | tailscale client   | <------------ |                                                |
 +--------------------+  POST /mcp    | cimon.exe (service account "cimon")            |
                      GET /artifacts  |   MCP endpoint · /ui  Artifact HTTP            |
                                      |   Job store (SQLite)  Intake worker (1)        |
                                      |   Recipe store (git)  Runner (1) -- spawns as  |
                                      |   Toolchain manager   Workspace manager        |
                                      |   Janitor             D:\cimon\recipes.git     |
                                      |        |                                       |
                                      |        v  build account "cimon-build"          |
                                      |   child processes in a Job Object              |
                                      +------------------------------------------------+
                                             | git fetch (read-only token)
                                             v
                                       GitHub: project repos
```

접속 경로는 **Tailscale 하나**다(§15.3). 서버는 `127.0.0.1` 에만 바인딩하고, 빌드 머신의 `tailscale serve` 가 tailnet 안에 HTTPS 로 내보낸다.

| 구성 요소 | 하는 일 | 상태를 두는 곳 |
|---|---|---|
| MCP 엔드포인트 | 도구 목록·호출, task 응답, `tasks/get`·`tasks/cancel` | 없음 — 작업 저장소에서 읽는다 |
| 아티팩트 HTTP | 산출물 다운로드, 링크 토큰 검증 | `artifacts\` |
| 리뷰 화면(`/ui`) | 제안 리뷰·반영, 이력, 레시피 탐색, 작업 상세 — [review-ui.md](review-ui.md), M9 | 작업 저장소 |
| 레시피 저장소 | 빌드 머신 안 git. 읽기(검증·겹치기·선택), 편집(검사 후 커밋), 제안·반영 조건·반영·되돌리기, 가져오기·내보내기, 번들 백업 | `recipes.git`, 메모리 캐시(커밋별) |
| 작업 저장소 | 작업·task·단계·산출물·툴체인 사용·warm 상태·제안 메타데이터·감사 로그 | `state\cimon.db`(SQLite, WAL) |
| 접수 작업자(1) | fetch → 커밋 고정 → 접수 검사 → 계획 → 툴체인 → 병합. 러너를 쓰지 않는다 | 작업 저장소 |
| 러너(1) | 워크스페이스 → 단계 → 수집. **빌드 계정으로** 자식 프로세스를 띄운다 | `logs\`, `homes\`, `tmp\` |
| 툴체인 관리자 | `main` 카탈로그대로 설치(다운로드·검증은 서비스 계정, 실행은 빌드 계정), base 검사 | `toolchains\` |
| 워크스페이스 관리자 | bare mirror, warm 슬롯·clean 트리(독립 클론), 형제 트리, mirror GC | `mirrors\`, `ws\` |
| 정리기 | 정리 규칙 표(§16.4)대로 지우기, 디스크 하한 감시, 시작 시 잔여물 정리 | — |

### 4.1 코드와 레시피

| | 내용 | 어디에 |
|---|---|---|
| `cimon` 저장소 | 서비스 코드, 이 설계, 레시피 형식 명세, 형식 예시 | GitHub. 프로젝트 정보가 없으므로 공개해도 된다 |
| 레시피 저장소 | `toolchains.toml`, `presets/`, `projects/<id>/`, 판정·검사 스크립트, 프로젝트별 노트, 레시피 안내서 | **빌드 머신 안** `D:\cimon\recipes.git`. 외부 원격 없음. 번들 백업을 빌드 머신 밖으로(§16.5) |

레시피 저장소는 평범한 git 저장소다 — 커밋으로 버전을 고정하고, 브랜치로 시도 실행을 하고, 이력으로 누가 무엇을 바꿨는지 남긴다. 다만 GitHub 같은 원격이 없고, 사람은 cimon 의 MCP 도구로 읽고 고친다(§6.4). 처음 내용은 `cimon recipe import --init` 으로 들여온다(§16.1).

### 4.2 코드 구조

Rust 워크스페이스. 실행 파일 하나(`cimon.exe`)가 서비스와 관리 명령을 겸한다.

```
cimon/
  Cargo.toml                 워크스페이스
  crates/
    cimon-core/              cimon.toml, 디스크 레이아웃, 판정·실패 분류·허용 조합, 식별자
    cimon-engine/            App(설정·저장소들), 작업 수명(생성·접수·provisioning·러너), 결과, 산출물, fixture,
                             반영 조건, 정리기, 백업
    cimon-recipe/            레시피 파싱·정적 검사·겹치기·선택·의미 차이. 순수 — 네트워크·프로세스 없음
    cimon-recipestore/       레시피 git 저장소: 읽기·편집 커밋·제안·반영 조건·반영·되돌리기·가져오기·내보내기·번들
    cimon-store/             SQLite 작업 저장소(rusqlite), 스키마 마이그레이션
    cimon-toolchain/         카탈로그, installer 구현(archive·github-release·rustup·uv·script·base), staging
    cimon-workspace/         git mirror·독립 클론·warm 슬롯·keep ref·GC
    cimon-runner/            빌드 계정 토큰, Job Object, 환경 구성, 단계 실행, fixture, 사전 정리, 수집
    cimon-judge/             판정기(exit·cargo-test·junit·script), 검사(apk-signer·pe-version·script)
    cimon-mcp/               rmcp 서버 처리기, 도구 정의, task↔작업 대응
    cimon-web/               리뷰 화면(M9) — axum 라우트, maud 템플릿, 포함된 정적 자원
    cimon/                   바이너리: serve · service · setup · doctor · drain · github-token · token · user · provision ·
                             recipe(check·show·import·export·restore), 숨은 도우미 __exec · __sweep · __account-env
  examples/recipes/          형식 예시(레시피 검사기의 시험 입력)
  docs/
```

`cimon-recipe` 를 순수하게 두는 이유: `recipe_edit` 의 저장 전 검사, 도구 호출의 입력 검증, 팀원 PC 의 `cimon recipe check <폴더>` 가 같은 코드로 같은 결론을 내야 한다.

---

## 5. 디스크 레이아웃

경로 길이 260자 함정을 피하려고 루트를 짧게 잡는다. Dev Drive(ReFS)에 둔다. 「서비스」 는 서비스 계정 `cimon`, 「빌드」 는 빌드 계정 `cimon-build` 의 권한이다(§15.1).

```
D:\cimon\                              서비스  빌드
  state\cimon.db                       rw      -      작업 저장소
  config\cimon.toml                    rw      -      설정
  config\link.key                      rw      -      산출물 링크 서명 키(cimon setup 이 만든다)
  config\build-account.dpapi           rw      -      빌드 계정 비밀번호(DPAPI, cimon setup)
  config\github-token.dpapi            rw      -      GitHub 읽기 토큰(DPAPI, cimon github-token)
  state\draining                       rw      -      있으면 새 작업 거절(cimon drain, 서비스 시작 때 지움)
  recipes.git                          rw      -      레시피 저장소(bare git) — 원본
  recipes\<commit>\                    rw      r      고정한 커밋의 레시피 체크아웃(${recipes})
  mirrors\<project>.git                rw      r      bare mirror(독립 클론의 alternates). gc.auto=0
  ws\<project>\warm-<n>\               rw      rw     증분 빌드용 독립 클론
  ws\<project>\j<id>\                  rw      rw     clean 작업용 일회 클론
  ws\<project>\<sibling id>\           rw      rw     「beside」 형제 트리
  ws\_sibling\<project>\j<id>\         rw      rw     「isolated」 형제 트리
  toolchains\<kind>\<ver>-<hash8>\     rw      r      managed 툴체인(카탈로그 항목 해시로 구분)
  toolchains\_staging\<id>\            rw      r*     설치 중 — 빌드 계정이 probe 를 실행(script 설치기만 그 동안 rw)
  toolchains\_bootstrap\               rw      r      rustup·uv
  caches\<project>\                    rw      rw     프로젝트별 캐시 — cargo·npm·uv·gradle·nuget
  homes\p\<project>\<NAME>\            rw      rw     project 범위 홈
  homes\j<id>\<NAME>\                  rw      rw     job 범위 홈
  artifacts\j<id>\                     rw      r      수집한 산출물 + provenance.json
  logs\j<id>\<step>\                   rw      rw*    단계 로그(서비스가 씀) + out\(빌드가 씀)
  logs\cimon\                          rw      -      서비스 로그
  tmp\j<id>\                           rw      rw     작업별 TEMP/TMP
  backup\                              rw      -      작업 저장소·레시피 저장소 백업(외부 사본은 §16.5)
```

루트(`D:\cimon` 자체)는 빌드 계정이 **목록만** 볼 수 있다(그 폴더만, 상속 없음) — 작업 트리에서 위로 올라가며 폴더를 읽는 빌드 도구(esbuild 등)가 루트에서 「Access is denied」 로 멈추지 않게. 보이는 것은 최상위 폴더 이름뿐이다.

`*` `logs\j<id>\<step>\` 안에서 빌드 계정이 쓸 수 있는 것은 `out\` 하위뿐이다 — `logs\` 는 서비스 전용이고, 러너가 단계마다 그 단계의 `out\` 에만 쓰기 권한을 준다. `toolchains\_staging\` 도 서비스 전용(빌드 계정은 읽기)이고, `script` 설치기의 staging 폴더만 그 설치 동안 쓰기 권한을 받는다 — 확인한 내려받기를 푸는 사이에 빌드 단계가 바꿔치지 못하게. script 가 만든 파일은 빌드 계정 소유라(소유자는 ACL 을 다시 쓸 수 있다) 옮기지 않고, 서비스가 링크·정션을 거부하며 최종 폴더로 **복사**한 뒤 그 복사본을 probe 한다.

`ws\` 아래 클론(`.git` 포함)은 빌드 계정이 만들고 다룬다 — 서비스는 그 안에서 git 을 돌리지 않는다(§11.1).

시도 브랜치에만 있는 새 프로젝트는 `<project>` 자리에 `<id>@<repo 해시 8자>` 를 쓴다 — 두 시도 브랜치가 같은 id 로 다른 저장소를 가리켜도 섞이지 않는다.

**디스크 예산**: 1단계 대상 프로젝트 중 가장 큰 것은 warm 트리 하나가 약 40 GB, 산출물은 작업당 수백 MB 다(프로젝트별 실측은 레시피 저장소 노트). **NVMe 2 TB 를 권장**한다.

---

## 6. 레시피

형식의 정본은 [recipe-spec.md](recipe-spec.md). 여기서는 서비스가 레시피를 어떻게 다루는지 적는다.

### 6.1 모델 요약

| 개념 | 뜻 | 명세 |
|---|---|---|
| `project.toml` | 저장소·허용 ref·형제·레시피 출처 | §3 |
| 단계(`[[step]]`) | 실행 단위. 종류 `setup`·`build`·`test`·`package`·`scenario` | §9 |
| 선택 | `build`·`test`·`run` 이 id·태그·산출물 이름·기본값으로 고르고, `after` 로 닫는다 | §14 |
| 능력(`needs`) | `display`·`device`·`gpu`. 러너에 없으면 제외하고 결과에 적는다 | §9, §14 |
| fixture | 여러 단계가 공유하는 떠 있어야 하는 것. 올리기·준비 대기·**반드시** 내리기·증거 보관 | §11 |
| 판정기(`judge`) | `exit`·`cargo-test`·`junit`·`script` | §12 |
| 파라미터 | 호출자가 고를 수 있는 값. 선언한 모양만, 적용 단계가 명시된 것만 | §8 |
| 계정 환경(`[account_env]`) | 예약 작업으로 뜨는 도구가 받을 빌드 계정의 사용자 환경 변수 | §7 |
| 프리셋 · 카탈로그 | 스택별 공통 단계 · 도구 종류별 설치 방식 | §17, §16 |

### 6.2 어디서 읽나

- **중앙 레시피**(`recipe_source = "central"`, 기본): 레시피 저장소의 `main`. 같은 머신의 git 이라 fetch 도, 묵을 일도 없다. `main` 이 움직이면(반영·되돌리기) 다음 작업부터 쓰인다.
- **프로젝트 레시피**(`recipe_source = "project"`): 빌드하는 ref 의 `.cimon/recipe.toml`. **형식은 정의하되 구현은 2단계**다.
- **시도 실행**: `recipe_ref` 로 레시피 저장소의 `recipe/*` 브랜치를 주면 그 브랜치의 `recipe.toml`·include·프리셋·판정 스크립트로 돈다. 결과에 `trial: true`. 단:
  - **이미 등록된 프로젝트의 `project.toml` 은 `main` 의 것을 쓴다.** 시도 브랜치에만 있는 새 프로젝트는 `repo` 가 `cimon.toml [recipes].allowed_repo_prefixes` 에 맞을 때만 받는다.
  - **툴체인은 `main` 카탈로그 항목으로 설치된 것만 쓴다.** 시도 브랜치의 카탈로그 항목이 `main` 과 다르면, 그 항목(해시)으로 이미 설치된 것이 있을 때만 쓴다. 없으면 접수에서 `inconclusive`/`environment` — 「시도 브랜치의 카탈로그 항목 `node` 가 main 과 다르다. 관리자가 검토 후 `cimon provision --recipe-ref <branch> --kind node`」. 시도 브랜치가 서비스 계정에 설치 스크립트를 실행시키지 못하게 하기 위해서다(원칙 7).
- 작업은 레시피 커밋·sha256 을 고정하고 결과에 적는다.

### 6.3 검증 — 정적 검사와 접수 검사

검사는 필요한 것에 따라 두 층으로 나뉜다(recipe-spec §15).

| 층 | 필요한 것 | 언제 돈다 |
|---|---|---|
| **정적 검사** | 레시피 저장소 내용만 | `recipe_edit`·`import` 저장 전, 도구 호출의 입력 검증, 팀원 PC 의 `cimon recipe check` |
| **접수 검사** | 프로젝트 저장소의 그 커밋, 서버 설정(`cimon.toml [base]`) | 작업의 접수 단계. 로컬 `recipe check` 는 이 검사를 「건너뜀 — 서버·저장소 필요」 로 명시한다 |

- **도구 호출 검증**: task 를 만들기 전에 `main`(또는 `recipe_ref` 브랜치)의 현재 커밋으로 정적 검사와 선택 규칙을 적용한다. **검증에 쓴 레시피 커밋을 작업에 고정**한다 — 대기하는 동안 레시피가 바뀌어도 검증받은 커밋으로 돈다.
- **저장을 막는 기준**: `recipe_edit` 은 저장 후 트리 전체를 검사하되, **이번 변경이 새로 만든 오류**(바뀐 파일·영향받는 프로젝트의 오류, 또는 base 에 없던 오류)만 저장을 막는다. base 에 이미 있던 다른 곳의 오류는 경고로 돌려준다 — 한 프로젝트의 문제가 다른 프로젝트의 편집을 막지 않게.
- `main` 의 레시피가 깨져 있으면(cimon 업그레이드로 검사가 엄격해진 경우 등) 그 프로젝트의 작업만 거절되고 다른 프로젝트는 영향받지 않는다. cimon 업그레이드 전에 새 실행 파일로 `main` 을 검사해 통과해야 한다(§16.6).

### 6.4 편집 — MCP 도구

레시피 저장소는 외부 원격이 없으므로, 읽고 고치는 길은 cimon 의 도구와 관리자 명령뿐이다.

| 도구 | 하는 일 | 권한 |
|---|---|---|
| `recipe_show` | 파일 하나 또는 디렉터리 목록을 ref(브랜치·커밋) 기준으로 | 팀원 |
| `recipe_export` | 한 ref 의 레시피 저장소 전체를 파일 묶음(경로→내용)으로 + 서버 cimon 버전. 로컬 검사·편집용 | 팀원 |
| `recipe_edit` | `recipe/*` 브랜치에 **여러 파일의 변경을 커밋 하나로**: `changes=[{path, content} \| {path, delete: true}]`, `base`(읽은 커밋), `message`. 정적 검사 후 저장(§6.3). 브랜치가 새것이면 `main` 에서 만들고 제안을 연다 | 팀원 |
| `recipe_diff` | 두 ref 의 파일 차이 | 팀원 |
| `recipe_history` | 파일·ref 의 커밋 이력 | 팀원 |
| `recipe_proposals` | 제안 목록(브랜치·상태·작성자·head·반영 조건 요약) — 시도 브랜치 목록을 겸한다 | 팀원 |
| `recipe_proposal` | 제안 하나: **의미 차이(위험 표시 포함)**, 반영 조건 상태, 시도 실행, 댓글 | 팀원 |
| `recipe_propose` | 제안의 제목·설명 | 작성자·관리자 |
| `recipe_comment` | 제안에 댓글 | 팀원 |
| `recipe_close` | 제안 닫기(사유) | 작성자·관리자 |
| `recipe_sync` | `main` 을 그 브랜치에 합친다(§6.5) | 팀원 |
| `recipe_promote` | 제안을 `main` 에 반영(§6.5) | 관리자 |
| `recipe_revert` | `main` 의 커밋 하나를 되돌린다(§6.5) | 관리자 |

규칙:

- **`main` 은 아무도 직접 고치지 않는다.** 모든 변경은 시도 브랜치 → 제안 → 반영으로 들어간다. 예외는 `recipe_revert`(되돌리기 사유를 남긴다)와 `cimon recipe import --init`(저장소가 비어 있을 때 한 번)뿐이다.
- **커밋 작성자는 토큰 사용자**다. CLI 의 커밋(`import`)은 `--author` 가 필수다. 메시지는 필수.
- **동시 편집**: `base` 를 받고, 브랜치 ref 를 `update-ref <ref> <new> <expected-old>` 로 바꾼다. 그 사이 움직였으면 `recipe_conflict` — 덮어쓰지 않는다.
- **쓰기 잠금 하나**: 편집·sync·반영·되돌리기·가져오기·정리기의 ref 삭제·번들 백업은 레시피 저장소 잠금 하나를 잡고 차례로 한다.
- **git ref 가 원본**이다. 브랜치 head·`main` 은 git 에서 읽는다. DB 의 제안 테이블은 메타데이터(제목·설명·상태·작성자·사유·댓글)만 갖는다.

사람이 편집기로 고치고 싶으면: `recipe_export` 로 받아 로컬에서 고치고 `cimon recipe check <폴더>`(서버와 같은 버전) → 바뀐 파일들을 `recipe_edit` 한 번으로 올린다(Claude 가 대신). 관리자는 `cimon recipe import <폴더> --branch recipe/<설명> --author <이름> --message …` 도 쓸 수 있다.

### 6.5 제안과 반영

**제안 = `recipe/*` 브랜치 하나 + 제목·설명·상태.** 제안마다 고유 id(`p0012`)가 있다.

```
recipe_edit(새 브랜치)  ──>  [open]  ──recipe_close──>  [closed]   브랜치 ref → refs/cimon/closed/<id>
                                │
                     recipe_promote(head 고정, 조건 충족 또는 사유)
                                │
                                v
                           [promoted]                              main fast-forward, 브랜치 ref 삭제
```

- 반영되면 브랜치 ref 를 지운다(커밋은 `main` 이력에 남는다). 닫으면 커밋을 `refs/cimon/closed/<id>` 로 옮기고 브랜치 ref 를 지운다. 그래서 **같은 브랜치 이름은 바로 다시 쓸 수 있고**, 다시 쓰면 새 제안(새 id)이 열린다.
- 열린 제안에 30일 동안 커밋도 시도 실행도 없으면 자동으로 닫고 댓글에 그 사실을 남긴다. 닫힌 제안의 커밋은 30일 뒤 정리한다.

**반영 조건**(`cimon-recipestore` 가 계산 — MCP·리뷰 화면이 같은 함수를 부른다):

| 조건 | 판정 | 덮어쓰기 |
|---|---|---|
| **정적 검사** | head 에 `main` 대비 새 오류가 없다(이미 있던 오류는 막지 않는다 — §6.3 과 같은 기준) | 불가 |
| **fast-forward** | `main` 이 제안의 기준 커밋 그대로인가. 아니면 `recipe_sync` 로 새 head 를 만들어야 한다 | 불가 |
| **변경 있음** | head 가 `main` 과 다르다(「변경 없음」 은 반영할 것이 없다) | 불가 |
| **시도 실행** | 의미 차이가 영향을 준 프로젝트마다, head 커밋으로 돈 시도 작업 중 **바뀐(추가·변경) 단계를 모두 포함한 작업**이 있고, 바뀐 단계를 **하나라도** 돌린 head 의 작업이 **모두** `pass` 인가(`cimon.toml [review]`) — 같은 head 에서 한 번이라도 실패했으면 더 넓은 재시도의 통과로 덮지 못한다. 바뀐 setup 은 그 작업에서 실제로 돌아(`skipped` 아님) `pass` 여야 한다 — warm 슬롯의 setup 지문에는 setup 이 실행하는 레시피 스크립트 내용이 들어가므로 스크립트가 바뀌면 다시 돈다. 프로젝트 전체 설정(툴체인·env·isolate·account_env·vars·workspace)이나 카탈로그 항목이 바뀌면 기본 `run`(기본 build + test)의 단계도 포함해야 하고, 기본 `run` 을 고를 수 없으면 미충족이다. setup 은 모든 작업에서 돌므로 setup 만 바뀌었으면 head 의 시도 작업 하나면 된다 | 사유를 적으면 가능 |
| **시도 실행으로 시험할 수 없는 변경** | 등록된 프로젝트의 `project.toml` 변경, 카탈로그 설치 항목 변경(관리자가 `provision --recipe-ref` 로 그 항목을 설치하고 시도 실행에 쓰였으면 시험된 것으로 친다), 이 러너에서 돌 수 없는(`needs`) 바뀐 단계, 의미 비교를 할 수 없는 변경(카탈로그가 해석되지 않음 등) | 사유를 적어야만 반영 |

- 의미 차이·영향받는 프로젝트는 겹친 레시피를 `main` 과 head 에서 비교해 만든다(review-ui §4.4 와 같은 함수). 프리셋·카탈로그·공용 스크립트가 바뀌면 그것을 쓰는 모든 프로젝트가 영향받는다. 스크립트는 **확장되는 모든 문자열**(단계·fixture·`[env]`·`clean_env`)의 **모든** `${recipes}` 참조로 찾고(두 구분자), 어느 레시피도 직접 가리키지 않는 바뀐 파일(스크립트가 불러 쓰는 헬퍼 등)은 스크립트를 참조하는 단계에 [실행]으로 붙는다. `default` 는 실효값으로 비교한다(test 가 기본 실행에서 빠지면 [동작·완화]).
- **`recipe_promote(proposal, head, override_reason?)`**: 잠금 → git 에서 브랜치 head 가 `head` 와 같은지(아니면 「제안이 그 사이 바뀌었다」 거절) → 조건을 **지금 다시 계산** → 반영자·사유를 먼저 기록 → `update-ref refs/heads/main <head> <기준 커밋>`(fast-forward 만) → DB 상태 `promoted` → 브랜치 ref 삭제 → 번들 백업. 리뷰어가 본 커밋과 다른 내용이 반영되지 않는다. `main` 이 움직인 뒤 죽으면 시작할 때 맞추기가 먼저 기록한 반영자·사유로 마무리한다.
- `base`·`head`(그리고 sync 의 `main`)는 **커밋 id(16진수)만** 받는다 — `recipe/x` 같은 이름은 늘 현재 head 와 같아 고정점이 되지 못한다. 브랜치 이름은 **소문자만**(Windows 의 ref 파일은 대소문자를 가리지 않아 `recipe/Foo` 와 `recipe/foo` 가 한 파일이 된다). 경로는 Windows 에서도 같은 파일을 가리켜야 한다 — 끝 점·공백, 장치 이름(`NUL`·`COM1`…), 대소문자만 다른 두 경로는 저장하지 않는다.
- **`recipe_sync(branch, head, main?, resolutions?)`**: `main` 을 브랜치에 합친 커밋을 만든다. 충돌이 없으면 그대로, 충돌이면 충돌 파일마다 base·`main`·브랜치 세 내용과 **그때의 `main` 커밋**을 돌려준다 — Claude 가 해결한 내용을 `resolutions={path: content}` 로, 받은 `main` 과 함께 다시 보내면 그것으로 병합 커밋을 만든다(`main` 이 그 사이 움직였으면 거절 — 다른 `main` 에 낡은 해결을 덮지 않는다). 병합 커밋도 정적 검사를 통과해야 저장된다. **새 head 이므로 시도 실행 조건은 다시 채워야 한다** — 시험하지 않은 조합이 `main` 에 들어가지 않는다.
- **`recipe_revert(commit, reason)`**: `main` 에 되돌리는 커밋을 바로 만든다(급한 복구). 정적 검사를 통과해야 하고, 이력·감사에 「되돌리기 — 사유」 로 남는다. 병합(sync) 커밋은 되돌리지 않는다 — `main` 에서 들어온 남의 변경까지 되돌리게 되므로 제안 자신의 커밋을 되돌린다. 이미 사라진 변경(빈 되돌리기)도 거절한다.
- **시작할 때 맞추기**: 열린 제안인데 브랜치 ref 가 없고 head 가 `main` 이력에 있으면 `promoted` 로, 그 밖에 브랜치가 없으면 `closed`(「브랜치 없음」)로 바꾼다. 반영 도중 서비스가 죽어도 제안이 영영 열린 채 남지 않는다.
- 리뷰 화면(M9)이 생기기 전에는 `recipe_proposal` 이 돌려주는 의미 차이·조건이 관리자의 리뷰 수단이다.

---

## 7. MCP 인터페이스

### 7.1 프로토콜

**MCP revision 2026-07-28 + Tasks 확장(`io.modelcontextprotocol/tasks`), Streamable HTTP, `POST /mcp`.** Claude Code 는 v2 런타임에서 이 revision 을 쓰는 서버에만 task 를 만든다(Claude Code v2.1.304 이상).

- **`initialize` 가 없다.** 요청마다 `_meta` 에 `io.modelcontextprotocol/protocolVersion`·`clientCapabilities`. 서버는 **`server/discover` 를 반드시 구현**하고 tasks 확장을 광고한다.
- **세션이 없다.** POST 만, 요청마다 `MCP-Protocol-Version`·`Mcp-Method`·`Mcp-Name` 헤더(`tasks/*` 는 `Mcp-Name` = taskId).
- **모든 결과에 `resultType`**(일반 결과는 `"complete"`).
- 1단계는 **새 revision 만** 받는다. 옛 revision 클라이언트는 `-32022` 로 명시적으로 실패한다.

**라이브러리 `rmcp` 3.x**(공식 Rust SDK, 2026-10-22 기준 3.7.0. 기능 `server` + `transport-streamable-http-server`).

- `call_tool` → `CallToolResponse::Task(CreateTaskResult)`, `get_task`·`cancel_task` 직접 구현, `enable_tasks()` 로 광고, `client_capabilities().supports_tasks()` 로 확장 선언 확인.
- **함정 1 — rmcp `TaskManager` 를 쓰지 않는다.** 메모리에만 두고, 기본 TTL **5분**에 안 끝난 작업을 **중단하고 `failed`** 로 만든다. 작업 저장소를 읽도록 직접 구현한다.
- **함정 2 — `allowed_hosts` 기본값이 loopback 뿐.** tailnet 호스트 이름을 넣는다.
- 함정 3 — 요청마다 처리기를 새로 만든다. 공유 상태는 `Arc` 뒤에.
- 인증 헤더는 `ctx.extensions` 의 `http::request::Parts` 에서 읽는다.
- 1단계는 task 를 `input_required` 로 만들지 않는다(승인 질문 없음 — §10.4). `tasks/update` 는 구현하지 않는다.

### 7.2 도구

| 도구 | task | 하는 일 |
|---|---|---|
| `list_projects` | | 등록된 프로젝트 |
| `describe_project` | | 겹친 레시피 요약 — 단계·산출물·파라미터·fixture·툴체인. `recipe_ref` 로 시도 브랜치 기준도 |
| `plan` | | `build`·`test`·`run` 이 무엇을 할지 **실행 없이** — fetch 하지 않고 현재 mirror 로. 알 수 없는 것(예: mirror 에 없는 커밋의 `version_file`)은 「알 수 없음 — 접수 때 확인」 으로 명시 |
| `build` · `test` · `run` | **●** | 단계 실행. `run` 이 기본형, `build`·`test` 는 고를 수 있는 종류를 좁힌 줄임 |
| `job_status` | | 작업의 상태·판정·요약·경고 |
| `job_log` | | 단계 로그의 끝 N 줄 또는 패턴 검색. 만료된 로그는 「<날짜>에 만료돼 지웠다」 |
| `artifacts` | | 산출물 목록 + **새로 발급한** 다운로드 링크 |
| `cancel_job` | | 작업 취소 요청 — 바로 돌아오고, 정리가 끝나면 `job_status` 가 `cancelled` 를 보인다(§7.5) |
| `queue` | | 접수·대기·실행 중 작업 |
| `recipe_show` · `recipe_export` · `recipe_edit` · `recipe_diff` · `recipe_history` | | 레시피 읽기·편집(§6.4) |
| `recipe_proposals` · `recipe_proposal` · `recipe_propose` · `recipe_comment` · `recipe_close` · `recipe_sync` · `recipe_promote` · `recipe_revert` | | 제안과 반영(§6.4, §6.5) |
| `web_login` | | 리뷰 화면 일회용 로그인 링크(M9, review-ui §8) |
| `scaffold_recipe` | **●** | 레시피 초안(등록하지 않는다, 저장소 코드를 실행하지 않는다). **2단계** |

툴체인 설치는 MCP 도구가 아니라 관리자 CLI `cimon provision` 이다(§10.4).

### 7.3 입력

공통 필드(`build`·`test`·`run`·`plan`):

```json
{
  "project": "example-app",
  "ref": "main",
  "recipe_ref": "recipe/add-desktop-test",
  "steps": ["build-apk"],
  "tags": ["android"],
  "artifacts": ["app-debug.apk"],
  "params": { "backend_url": "http://192.168.0.20:8800" },
  "clean": false
}
```

| 필드 | 기본 | 뜻 |
|---|---|---|
| `project` | 필수 | `list_projects` 의 id |
| `ref` | 프로젝트 `default_ref` | 브랜치 이름 또는 커밋 SHA. 허용 목록(glob)에 안 맞으면 즉시 거절. 맞지만 없는 브랜치, 허용 브랜치에서 닿지 않는 커밋은 접수에서 `fail`/`request`(§8.2) |
| `recipe_ref` | `main` | 레시피 저장소의 `recipe/*` 브랜치 — 시도 실행(§6.2) |
| `steps` · `tags` | 아래 표 | 둘을 함께 주면 **합집합**(recipe-spec §14) |
| `artifacts` | 선택된 단계의 `default = true` 산출물 | 이름 목록. `[]` 면 수집 안 함. 이름을 대면 그 생산 단계가 **도구의 후보 종류와 상관없이** 들어간다 |
| `params` | 각 `default` | 선언된 것만. **선택된 어느 단계에도 적용되지 않는 파라미터를 명시하면 거절**(`param_not_applicable`) |
| `clean` | `false` | warm 트리 대신 일회 클론(§11.4) |

| 도구 | `steps`·`tags` 로 고를 수 있는 종류 | 아무것도 고르지 않으면 |
|---|---|---|
| `build` | `build` · `package` | `default = true` 인 build |
| `test` | `test` · `scenario` | `default = true` 인 test |
| `run` | `build` · `package` · `test` · `scenario` | `default = true` 인 build + test |

어느 도구든 `after`·fixture 의존과 `artifacts` 의 생산 단계는 종류와 상관없이 따라 들어온다. 고를 수 없는 종류의 id 를 `steps` 에 주면 `build`·`test` 는 `wrong_kind` 로 거절한다. **제외(recipe-spec §14)까지 적용한 뒤 setup 말고 돌 단계가 하나도 없으면 거절**한다(`nothing_to_run`).

`plan` 은 위에 `"tool": "build" | "test" | "run"` 을 더한다. `job_status`·`job_log`·`artifacts`·`cancel_job` 은 `{"job": "j000123"}`(작업 번호 또는 task_id), `job_log` 는 `step`·`tail`(기본 100)·`grep` 을 더한다.

### 7.4 결과

완료 결과(`structuredContent`):

```json
{
  "job_id": "j000123",
  "task_id": "t_5q2m8x0v3k7c9r1n4b6z8w2y0p",
  "done": true,
  "tool": "run",
  "verdict": "pass",
  "failure_class": null,
  "summary": "example-app main(3f2a91c) run 통과 — build-apk, test-shared. 산출물 1개. 경고 1.",
  "warnings": [ { "source": "fixture", "message": "sim 인스턴스가 down 뒤에도 남아 작업 끝에 정리했다" } ],
  "project": "example-app",
  "ref": "main",
  "commit": "3f2a91c…",
  "siblings": [],
  "trial": false,
  "deduplicated": false,
  "recipe": { "source": "central", "ref": "main", "commit": "a1b2c3d…", "sha256": "…" },
  "workspace": { "mode": "warm", "slot": 1 },
  "params": { "backend_url": "http://192.168.0.20:8800" },
  "excluded": [],
  "toolchains": { "rust": "1.94.0", "jdk": "17.0.19+10" },
  "steps": [
    { "id": "build-apk", "kind": "build", "result": "pass", "seconds": 190, "log": "j000123/build-apk" },
    { "id": "test-shared", "kind": "test", "result": "pass", "seconds": 64, "tests": { "passed": 212, "failed": 0, "skipped": 3 } }
  ],
  "artifacts": [
    {
      "name": "app-debug.apk",
      "bytes": 28311552,
      "sha256": "…",
      "inspect": { "apk-signer": { "cert_sha256": "…", "subject": "CN=Android Debug" } },
      "url": "https://cimon.<tailnet>.ts.net/artifacts/j000123/app-debug.apk?t=…",
      "url_expires": "2026-10-26T15:00:00Z",
      "retained_until": "2026-11-02T14:00:00Z"
    }
  ],
  "download_hint": "Invoke-WebRequest <url> -OutFile <path>; (Get-FileHash <path>).Hash 를 sha256 과 대조"
}
```

- `toolchains` 값은 예시다. 실제 값은 작업마다 실행해서 읽은 버전이다.
- **출력 스키마(`outputSchema`)는 선언하지 않는다.** 선언하면 거절 결과(`isError`, `{error, message}`)까지 그 스키마에 맞아야 한다. 결과의 모양은 이 절이 정본이고, 성공·「접수됨」(§7.5) 결과는 언제나 `job_id`·`done`·`summary` 를 싣는다.
- **Claude 가 알아야 할 것은 전부 `structuredContent` 에** — 요약도 `summary` 로. 같은 요약을 `content` 텍스트에도 싣는다(Agent SDK 문서는 `structuredContent` 가 있으면 `content` 를 넘기지 않는다고 한다. 원격 서버도 그런지는 확인 안 함).
- 실패면 `summary` 첫 줄에 실패 분류·실패한 단계·**첫 에러 줄**. `excluded`·`warnings` 가 있으면 그 수를 적는다.
- 리뷰 화면이 켜져 있으면(`[web].enabled`) `job_url` 을 싣는다. 꺼져 있으면 싣지 않는다.
- 산출물은 `resource_link` 블록으로도 싣는다.

### 7.5 요청(task)과 작업의 대응

**호출 하나 = task 하나, task 는 작업 하나를 따른다.** 중복 병합(§8.3)되면 **여러 task 가 한 작업을 따른다**(다대일). 식별자는 둘: `job_id`(작업마다)와 `task_id`(128비트 난수, 호출마다 — Tasks 확장은 task ID 가 **추측 불가**일 것을 요구한다). 둘 다 **인증된 팀원이면 누구나** 조회·취소할 수 있다.

필드 규칙:

- `CreateTaskResult` 와 `tasks/get` 결과에 **`taskId`·`status`·`createdAt`·`lastUpdatedAt`·`ttlMs` 가 모두** 있어야 한다. 하나라도 없으면 Claude Code 는 `Unsupported result type 'task'` 로 실패한다.
- `tasks/get` 의 `resultType` 은 `"complete"`, 완료면 `result` 에 `CallToolResult` 전체.
- **`statusMessage` 는 매번 싣는다** — 진행을 알리는 유일한 수단이고, 빠지면 Claude Code 가 `/tasks` 표시를 지운다.
- **DB 에 커밋한 뒤에 task 를 돌려준다.** 작업 저장소는 `synchronous=FULL`(§13).

| 작업 상태(§8.1) | task `status` | `statusMessage` 예 |
|---|---|---|
| intake | working | `접수: git fetch` |
| provisioning | working | `툴체인: node 24.18.0 설치 중` |
| ready | working | `대기열 2번째 — 앞: j000121 example-app build` |
| running | working | `build-apk (1/2) · 3분 10초` |
| collecting | working | `산출물 수집 1/1` |
| cancelling | working | `취소 중 — fixture 내리는 중(admin-a 요청)` |
| done | completed | — |
| merged | (따르는 작업의 상태) | `j000118 과 같은 요청 — 그 작업을 따른다` |
| cancelled | cancelled | `admin-a 가 취소함` / `요청한 task 가 모두 떨어짐` |
| queue_timeout | cancelled | `12시간 동안 시작하지 못해 취소함` |
| interrupted | completed(`isError: true`) | `서비스 재시작으로 중단됨` |

시간·수명:

- **응답을 먼저.** Claude Code 의 요청별 타이머는 첫 응답 바이트까지 돌고 값은 60초·서버 도구 타임아웃·`MCP_TIMEOUT` 중 가장 큰 것. `tools/call` 은 입력 검증과 DB 커밋까지만 하고 task 를 **JSON 한 번**으로 돌려준다. 프로젝트 fetch 는 접수 단계에서 한다. 동기 도구(`plan`·`describe_project`·`recipe_*`)는 fetch 하지 않는다.
- `pollIntervalMs` 5000.
- **`ttlMs` = 작업 레코드 보관 기간(30일).**
- 모르는·만료된 ID 는 `-32602`. 보관 기간 안의 레코드는 재시작에도 잃으면 안 된다.

취소:

- **`tasks/cancel`(task 하나)** → 그 task 를 작업에서 **뗀다**. 다른 task 가 남아 있으면 작업은 계속 돈다. 하나도 안 남으면 작업 취소를 요청한다.
- **`cancel_job`(작업)** → 작업 취소를 요청한다. 바로 돌아오고(「취소를 요청했다 — 정리가 끝나면 job_status 가 cancelled」), 작업은 `cancelling` 을 거쳐 Job Object 종료·fixture `down`·빌드 계정 잔류 프로세스 정리 뒤 `cancelled` 가 된다. 이미 끝난 작업은 바꾸지 않는다.

task 를 못 쓰는 클라이언트(서브에이전트, VS Code 확장, `claude -p`):

- 막고 기다리지 않는다. 작업을 만들고 일반 결과 `{"job_id": "…", "done": false, "accepted": true, "summary": "아직 끝나지 않았다. job_status 로 확인하라. 시작 전 작업은 12시간까지 기다린다"}`.

### 7.6 결과 크기

Claude Code 는 텍스트 결과가 5만 자를 넘으면 파일로 빼고, `isError` 결과는 약 1만 1천 자를 넘으면 가운데를 자른다.

- 작업 결과는 **8천 자 이하.** 실패면 분류·실패 단계·첫 에러 줄과 앞뒤 몇 줄·로그 위치를 앞에.
- 긴 로그는 `job_log` 의 `tail`·`grep` 으로만. `recipe_export` 는 크면 파일로 빠지는 것을 전제한다.
- 산출물은 결과에 넣지 않는다 — HTTP(§12).

### 7.7 거절

task 를 만들기 전(또는 동기 도구에서) 거절하는 경우. `isError: true` 일반 결과로 돌려준다.

| 코드 | 언제 | 메시지 예 |
|---|---|---|
| `unauthorized` · `forbidden` | 토큰 없음·틀림 / 권한 없음 | (HTTP 401) / `반영은 관리자만 — 관리자에게 요청하라` |
| `unknown_project` | | `프로젝트 "eb" 가 없다. list_projects: …` |
| `ref_not_allowed` | 허용 목록(glob) 밖 | `"release/x" 는 허용 ref 가 아니다: main, feature/**, fix/**` |
| `recipe_invalid` | 정적 검사 실패(호출 검증·`recipe_edit` 저장 전) | `projects/example-app/recipe.toml:42: step "test-shared": unknown key "timeot_min"` |
| `unknown_recipe_ref` · `trial_ref_not_allowed` | 없는 시도 브랜치 / `recipe/*` 밖 | `recipe/x 가 없다 — recipe_proposals 로 확인하라` |
| `main_not_editable` | `recipe_edit` 대상이 `main` | `main 은 직접 고치지 않는다 — recipe/<설명> 브랜치에서 고치고 반영하라` |
| `recipe_conflict` | `base`·`head` 이후 브랜치가 움직임 | `recipe/x 는 그 사이 9f3b2e4 로 바뀌었다 — 다시 읽고 고쳐라` |
| `not_fast_forward` | 반영 시 `main` 이 움직임 | `main 이 b40c9d1 로 움직였다 — recipe_sync 로 맞춘 뒤 시도 실행을 다시` |
| `promote_conditions_unmet` | 덮어쓸 수 없는 조건 미충족, 또는 사유 없이 덮어쓰기 시도 | `시도 실행: example-app 의 바뀐 단계 test-shared-desktop 을 포함한 head 실행이 없다 — 돌리거나 override_reason 을 적어라` |
| `proposal_closed` | 닫힌·반영된 제안 조작 | `p0012 는 반영됐다 — 같은 이름으로 recipe_edit 하면 새 제안이 열린다` |
| `unknown_step` · `wrong_kind` · `unknown_tag` · `unknown_artifact` | | `"test-shared" 는 test 단계다 — test 나 run 으로 부르라` |
| `unknown_param` · `invalid_param` · `param_not_applicable` | | `backend_url 은 선택된 단계(package)에 적용되지 않는다 — build-apk 에만 쓰인다` |
| `needs_unavailable` · `nothing_to_run` | 능력 없는 단계를 이름으로 고름 / 제외 뒤 돌 단계 없음 | `고른 단계 1개가 모두 제외됐다(ui-test — display 필요)` |
| `queue_full` | 그 사용자의 시작 전 작업이 한도 이상 | `시작 전 작업이 5개다(j000130 …)` |
| `disk_low` · `draining` | 정리 후에도 여유 부족 / 업그레이드 대기 | |

---

## 8. 작업 모델

### 8.1 상태 — 접수와 실행을 나눈다

```
[접수 — 러너를 쓰지 않는다, 작업자 1]          [실행 — 러너 1개]
intake --> provisioning --> ready --> running --> collecting --> done(verdict)
   |            |             |          |            |
   +------------+-------------+          +------------+--> cancelling --> cancelled
   |  (시작 전)                 |                            (취소 요청)
   +--> cancelled | queue_timeout          running·collecting --> interrupted (서비스 재시작)
   intake 끝 --> merged (2차 병합)
```

| 상태 | 하는 일 | 러너 |
|---|---|---|
| `intake` | 프로젝트·형제 mirror fetch → ref 를 커밋으로 고정·허용 브랜치 확인 → **접수 검사**(version_file 읽기·일치, 프로젝트 레시피면 그 검사) → 선택·닫기·제외 → **계획 기록** → 2차 병합 | 안 씀 |
| `provisioning` | managed 설치(`main` 카탈로그)·probe, base 검사. 자동 설치가 허용되지 않은 도구가 없으면 멈추고 알린다(§10.4) | 안 씀 |
| `ready` | 러너 대기. 순서는 **요청 시각** | — |
| `running` · `collecting` | 워크스페이스 → 단계 → 수집(§9.1) | 씀 |
| `cancelling` | 취소 요청을 받아 정리 중 | 씀(실행 중이었으면) |
| `merged` | 2차 병합으로 다른 작업에 합쳐졌다 — 접수 끝에서만 | — |
| `queue_timeout` | 시작 전 상태로 12시간을 넘겼다 | — |

**접수와 실행을 나눈 이유**: 러너를 잡기 전에 끝낼 수 있는 일(fetch, 접수 검사, 툴체인 설치)을 러너를 잡은 뒤에 하면 러너가 놀고, 커밋을 모르니 커밋 기준 병합을 할 수 없다. 접수 작업자는 **하나**다 — 설치 경합이 없고, 1단계 규모에 충분하다. 접수와 러너는 동시에 돈다(네트워크·디스크 일 대 빌드). 정리기는 끝나지 않은 작업이 쓰는 툴체인·클론·mirror 객체를 지우지 않는다(§11.1, §16.4).

### 8.2 판정과 실패 분류

| 판정 | 언제 |
|---|---|
| `pass` | 돈 단계(setup 제외, 하나 이상)가 전부 `pass` 이고, 요청한 산출물이 전부 수집됨 |
| `fail` | 해야 할 일을 못 했고 그 원인이 코드·레시피·요청에 있다 |
| `inconclusive` | 판정에 필요한 것을 얻지 못했다 — 환경·인프라·관측 문제 |

**실패 분류(`failure_class`)와 허용 조합**:

| 분류 | 허용 판정 | 예 | 누가 볼 일 |
|---|---|---|---|
| `code` | `fail` | 컴파일 에러, 테스트 실패, 0개 실행, 금지 출력, 산출물 개수 불일치, 시간 초과, 브랜치가 지은 실행 파일로 fixture 기동 실패(`on_up_failure = "code"`), fixture 의 서비스가 기동 중 종료 | 브랜치 작성자 |
| `recipe` | `fail` · `inconclusive` | `fail`: 접수 검사 실패(`version_file_missing`·`version_file_mismatch`·`glob_matches_tracked`). `inconclusive`: 판정·검사 스크립트가 죽거나 틀린 JSON·허용 밖 조합을 냄, `required` 검사 실패 | 레시피 담당 |
| `request` | `fail` | 없는 브랜치(`ref_not_found`), 허용 브랜치에서 닿지 않는 커밋(`commit_not_allowed`) | 호출자 |
| `environment` | `inconclusive` | base 툴체인 없음, 자동 설치가 허용되지 않은 도구 없음, 시도 브랜치 카탈로그 항목 미설치, 디스크 부족 | 관리자 |
| `infra` | `inconclusive` | fetch·다운로드 실패, fixture 등록 실패, 서비스 재시작 | 관리자 또는 재시도 |
| `unobserved` | `inconclusive` | 관측 창 안에 응답 없음, 관측 데이터 없음 | 시험 설계 |

- 작업의 판정은 단계 판정의 최악이다(`fail` > `inconclusive` > `pass`). **작업의 실패 분류는 작업 판정과 같은 판정을 낸 첫 단계의 것**이다 — 「인프라 문제 하나 + 진짜 회귀 하나」 가 「인프라, 재시도」 로 보이지 않게.
- 판정 스크립트가 허용 조합 밖의 쌍을 내면 `inconclusive`/`recipe`.
- 접수 단계의 실패는 그 이유 코드(`ref_not_found`·`commit_not_allowed`·`version_file_missing`·`toolchain_not_provisioned` …)를 `summary` 첫 줄에 싣는다.
- 제외(`excluded`)·setup 생략·경고는 판정을 바꾸지 않는다. 결과에 적힌다.
- `pass` 가 아니면 `isError: true`.

### 8.3 대기열

- **순서**: 러너 하나. `ready` 작업을 요청 시각 순서로.
- **중복 병합 — 접수 끝에서 한 번**: 키 `(project, commit, 형제 커밋들, 레시피 커밋·sha256, 도구, 정렬한 steps·tags·artifacts, 기본값을 채운 params, clean)` 이 같은 작업이 `ready`·`running` 이면 이 작업을 `merged` 로 끝내고 task 들을 그 작업에 붙인다. 키에 결과를 바꾸는 입력이 모두 들어 있으므로 같은 결과다. 끝난 작업과는 병합하지 않는다. 병합된 결과에는 `deduplicated: true` 와 따르는 작업 번호.
  - 호출 시점(커밋 확정 전)의 병합은 하지 않는다 — push 직후 다시 요청한 사람에게 옛 커밋의 결과를 주게 되기 때문이다.
- **시작 전 작업의 상한**: 아무도 보지 않아도 유지한다(원칙 5). 시작 전 상태로 12시간(`max_queue_hours`)을 넘기면 `queue_timeout`. 사용자당 시작 전 작업이 5개(`max_queued_per_user`) 이상이면 새 호출을 `queue_full` 로 거절.
- 디스크 여유가 하한 아래면 정리기를 먼저 돌리고, 그래도 모자라면 거절.

### 8.4 서비스 재시작

- 접수 단계 작업은 접수를 **처음부터 다시** 한다(모두 다시 해도 같은 결과).
- `ready` 작업은 그대로 기다린다.
- `running`·`collecting`·`cancelling` 이던 작업은 `interrupted`(`inconclusive`/`infra`, 취소 중이었으면 `cancelled`). 그 warm 슬롯의 setup 상태를 지우고(§11.3), 빌드 계정 잔류 프로세스·예약 작업과 fixture 를 정리한다(§9.2).
- 시작 정리(§16.4): 끝난 작업의 `tmp\`·job 범위 홈·clean 클론·형제 트리 잔여물, 제안·브랜치 맞추기(§6.5).

---

## 9. 실행

### 9.1 작업 하나의 순서

1. **접수**(러너 밖, §8.1). 여기까지 끝난 작업만 `ready` 가 된다.
2. **시작 점검**(러너): 빌드 계정 잔류 프로세스·예약 작업이 없는지 확인하고, 있으면 정리하고 경고를 남긴다.
3. **계정 환경**: 레시피의 `[account_env]` 값을 빌드 계정의 사용자 환경 변수로 쓴다(§9.2).
4. **워크스페이스**: warm 슬롯 고르기·확인·갱신 또는 clean 클론, 형제 트리(§11). 이 git 은 빌드 계정으로 돈다.
5. **running**: 단계마다
   1. 환경 구성(§9.4)
   2. 요청된 산출물과 판정 리포트의 **사전 정리**(§12.2, §9.6)
   3. 그 단계가 쓰는 fixture 가 아직 안 떴으면 `up` → `ready`
   4. 빌드 계정으로, Job Object 안에서 실행, 시간 제한
   5. 판정기 실행 → 단계 결과
   6. 산출물 수집(서비스 계정이 복사·sha256·검사)
   7. 그 fixture 를 쓰는 마지막 단계였으면 증거 보관(`collect`) → `down` → `gone` 확인
6. **collecting**: `provenance.json`, 링크 발급.
7. **done**: 판정·경고 기록. 남은 fixture 정리, Job Object 종료, **빌드 계정 잔류 프로세스·예약 작업 정리·확인**, `[account_env]` 값 지우기, `tmp\j<id>\` 삭제.

**한 작업 안의 단계는 차례로 돈다.** 큰 C++ 빌드 하나가 CPU·RAM 을 다 쓰고, 로그가 섞이지 않아 실패 원인을 한 단계에서 읽을 수 있다. 가벼운 단계끼리 병렬은 §17.

### 9.2 프로세스와 계정

- 러너는 단계마다 **빌드 계정 `cimon-build` 의 토큰으로** 자식 프로세스를 띄운다(`LogonUser`(batch) + `LoadUserProfile` + `CreateProcessAsUserW`, 일시정지로 만든 뒤 Job Object 에 넣고 재개, 상속 핸들은 표준 입출력 셋만). 띄우는 것은 `cimon.exe __exec <spec>` 도우미이고, 도우미가 Rust 표준 라이브러리로 실제 명령을 띄운다(`.bat`·`.cmd` 인자 이스케이프를 표준 라이브러리에 맡기기 위해). 빌드 계정 비밀번호는 `cimon setup` 이 무작위로 정해 **DPAPI(머신 범위)로 암호화해 `config\build-account.dpapi`** 에 둔다 — `config\` 는 서비스 계정과 관리자만 읽는다. 서비스 계정에는 `setup` 이 「프로세스 수준 토큰 바꾸기」·「프로세스의 메모리 할당량 조정」과 **「파일 및 디렉터리 백업」·「복원」** 을 준다(서비스가 시작할 때 켠다 — 서비스는 빌드 계정 비밀번호를 쥔 신뢰 쪽이라 이 권한을 더해도 경계가 바뀌지 않는다). **프로필**: 사용자 프로필 서비스는 이 두 권한이 있어도 관리자가 아닌 호출자의 `LoadUserProfile` 을 거부한다(`ERROR_ACCESS_DENIED`, 실제 머신에서 확인). 그래서 프로필은 `setup`(관리자)이 한 번 만들고, 서비스는 `LoadUserProfile` 이 거부되면 프로필의 `NTUSER.DAT` 를 `RegLoadKey` 로 `HKEY_USERS\<SID>` 에 직접 올린다 — 이것은 두 권한만 요구하고, 그 계정의 프로세스는 HKCU 를 거기서 찾는다(`[account_env]` 가 이 하이브에 쓴다). 자식은 서비스의 창 스테이션을 물려받지 않는다(`lpDesktop` = 빈 문자열). 파이프 핸들은 `CreateProcessAsUserW` 직전에만 상속 가능하게 해 다른 스레드가 띄우는 프로세스로 새지 않게 하고, 출력 읽기는 프로세스가 끝난 뒤 10초를 넘기면 끊는다. 도우미가 명령을 못 띄우면 그 사유를 따로 알려 종료 코드가 아니라 「시작 실패」(`inconclusive`/`infra`)가 된다. 2026-10-26 실제 Windows PC 에서 이 경로로 설치 스크립트와 작업 하나를 끝까지 돌렸다(setup.md 「실제 머신에서 확인한 것」).
- 서비스 계정은 가상 계정 **`NT SERVICE\cimon`** 이다(관리할 비밀번호가 없다). 이 문서의 「서비스 계정 `cimon`」 은 이것을 가리킨다.
- Job Object(`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, breakaway 불허). 취소·시간 초과는 Job Object 통째로 종료. 단계가 끝난 뒤 남은 프로세스는 종료하고 경고로 남긴다.
- **Job Object 로 못 잡는 것**: 작업 스케줄러·서비스로 띄운 프로세스. 프로젝트 도구가 스택을 「호출자보다 오래 살게」 띄우는 경우가 그렇다. 그래서:
  - **작업이 끝날 때마다 빌드 계정이 소유한 모든 프로세스를 찾아 종료하고, 남은 것이 없는지 확인한다.** 빌드 계정이 등록한 예약 작업도 지운다. 남았던 것은 `warnings` 에 적는다. 프로세스는 Toolhelp 스냅숏과 각 프로세스 토큰의 SID 로 고른다(tasklist·wmic 은 출력이 지역화되고 wmic 은 현재 Windows 에 없다). 스윕과 빌드 계정으로 도는 설치 스크립트·probe 는 한 잠금으로 겹치지 않게 해 스윕이 설치를 죽이지 않는다 — 이 잠금은 `state\build-account.lock` 을 독점으로 열어 프로세스 사이에도 걸린다(`cimon provision` 이 서비스 옆에서 돈다). 작업 전체가 아니라 스윕 동안만 잡으므로 접수를 막지 않고, 단일 계정 모드에서는 잡지 않는다.
  - 다음 작업 시작 전에 다시 확인한다.
  - 빌드 계정은 빌드만 하는 계정이라, 그 계정의 프로세스를 모두 죽여도 다른 것이 깨지지 않는다 — 계정을 나눈 이유 중 하나다(러너가 하나라 성립한다).
- **계정 환경(`[account_env]`)**: 예약 작업으로 뜬 프로세스는 호출자의 환경이 아니라 **계정에 저장된 사용자 환경**을 받는다. 레시피가 `[account_env]` 에 적은 변수는 작업 시작 전에 cimon 이 **빌드 계정으로 작은 도우미를 실행해** 그 계정의 사용자 환경 변수(HKCU\Environment)로 쓰고, 작업 끝에 지운다. 값은 같은 이름의 `[isolate]` 경로와 같게 해 직접 실행한 단계와 예약 작업이 같은 홈을 본다. 작업 스케줄러가 새 프로세스에 이 값을 넣어 주는지는 **확인 필요**(M7).
- 예약 작업을 S4U 로 등록하는 도구는 그 계정에 「일괄 작업으로 로그온」 권한이 있어야 돈다. 빌드 계정에 준다. `doctor` 가 검사한다.

### 9.3 명령 해석

- `run` 은 argv 배열이고 셸을 거치지 않는다.
- **첫 원소에 경로 구분자가 있을 때만**(예: `./gradlew.bat`) 작업 폴더 기준으로 찾는다. 이름만 있으면 구성한 `PATH` 에서만 찾는다 — 저장소가 커밋한 `cargo.exe` 같은 파일이 고정한 도구를 가로채지 못하게.
- `.bat`·`.cmd` 는 Rust 표준 라이브러리의 배치 파일 실행(인자 이스케이프 포함)에 맡긴다. `cmd.exe` 가 인자를 다시 해석한다는 점은 남는다 — 파라미터 값의 모양 검증이 그 부분을 막는다.

### 9.4 환경 변수

자식 환경은 **빈 상태에서 새로 만든다.** 순서대로 겹친다.

1. 시스템 기본값: `SystemRoot`·`windir`·`ComSpec`·`PATHEXT`·`OS`·`SystemDrive`·`ProgramFiles`·`ProgramFiles(x86)`·`ProgramW6432`·`ProgramData`·`CommonProgramFiles`·`CommonProgramFiles(x86)`·`CommonProgramW6432`·`NUMBER_OF_PROCESSORS`·`PROCESSOR_*`·`COMPUTERNAME`
2. 빌드 계정의 사용자 값: `USERPROFILE`·`HOMEDRIVE`·`HOMEPATH`·`LOCALAPPDATA`·`APPDATA`·`USERNAME`·`USERDOMAIN`
3. `TEMP`/`TMP` → `tmp\j<id>\`
4. `PATH` = 계획이 쓰는 툴체인의 `bin`(레시피 선언 순서) + `System32` 계열
5. 툴체인 `exports`
6. 캐시: `caches\<project>\` 아래로 `CARGO_HOME`·`npm_config_cache`·`UV_CACHE_DIR`·`GRADLE_USER_HOME`·`NUGET_PACKAGES`. `RUSTUP_HOME` 은 공유·읽기 전용
7. `[isolate]` 홈
8. 형제 환경 변수
9. 레시피 `[env]` → (clean 작업이면) `[workspace].clean_env` → fixture `env` → 단계 `env`
10. 파라미터 `env`(그 단계가 `applies_to` 에 있을 때)

- 9·10 에서 **아래 층의 키를 위 층이 통째로 바꾸면** 정적 검사가 경고를 낸다(예: `clean_env` 의 `GRADLE_OPTS` 가 `[env]` 의 `GRADLE_OPTS` 를 지움). 값을 합치지 않으므로 필요한 값을 위 층에 다 적는다.
- **주지 않는 것**: `CARGO_TARGET_DIR`, `RUSTFLAGS` 는 레시피가 명시하지 않는 한 주지 않는다. 비밀은 어떤 경로로도 주지 않는다.

### 9.5 출력

- stdout·stderr 는 단계 로그 파일로 그대로(서비스가 씀). 단계 로그는 **200 MB 상한** — 넘으면 앞 50 MB·뒤 50 MB 를 남기고 잘린 사실을 경고로. 메모리에는 끝 200줄.
- 「첫 에러 줄」 은 툴체인별 기본 패턴 + 레시피 `error_patterns`.
- `fail_on_output` 에 걸린 줄이 있으면 종료 코드가 0 이어도 `fail`/`code`.

### 9.6 판정기 · 검사

| 판정기 | 구현 메모 |
|---|---|
| `exit` | 종료 코드. **테스트 개수를 세지 못하므로 test·scenario 단계에 쓰면 정적 검사 경고**(「0개 실행을 못 잡는다」) — 레시피가 `uncounted = true` 로 알고 있다고 밝히면 경고하지 않는다 |
| `cargo-test` | 종료 코드 + `test result:` 줄 합산. `passed < min_tests` 면 `fail`/`code` |
| `junit` | 종료 코드 + **단계 전에 `reports` glob 을 지운 뒤** 새로 생긴 XML 만 합산. 미리 지우는 이유: 증분 빌드 도구가 테스트를 「이미 최신」 으로 건너뛰는데 리포트가 남아 있으면 그 상태가 유지된다. 옛 리포트를 인정하면 조용한 통과가 된다 |
| `script` | 레시피 저장소의 판정 스크립트를 빌드 계정으로. 출력 JSON `{"verdict", "failure_class", "reason", "metrics"}`. **죽거나 JSON 이 틀리거나 허용 조합(§8.2) 밖이면 `inconclusive`/`recipe`** |

| 검사 | 구현 메모 |
|---|---|
| `apk-signer` · `pe-version` | 내장 |
| `script` | 레시피 저장소의 검사 스크립트(빌드 계정). 출력 JSON 을 `inspect` 에 |
| 실패하면 | `required = true` 인 검사는 그 단계를 `inconclusive`/`recipe` 로. 아니면 산출물에 `inspect_error` 를 적고 `warnings` 에 |

판정·검사 스크립트가 서비스 바깥(레시피 저장소)에 있는 이유: 특정 프로젝트의 결과 형식을 아는 코드를 서비스에 넣지 않기 위해서다(원칙 1).

---

## 10. 툴체인

### 10.1 머신 준비 때 설치하는 것

`cimon setup`(관리자 1회)이 `toolchains\_bootstrap\` 에 **rustup·uv** 를 설치한다. 버전은 `cimon.toml [bootstrap]` 에 **반드시 고정**한다 — 비어 있거나 버전 모양이 아니면 setup 의 그 단계가 실패한다(「최신」 을 조용히 받지 않는다). rustup-init 은 `static.rust-lang.org/rustup/archive/<버전>/` 에서 같은 자리의 sha256 으로, uv 는 그 버전의 GitHub 릴리스 자산 digest 로 확인한다.

### 10.2 두 층

| 층 | 무엇 | 누가 |
|---|---|---|
| **base** | 관리자 권한·대용량·재부팅·EULA 가 필요한 것(예: Visual Studio Build Tools, Git for Windows, Android SDK) | 관리자, 미리 |
| **managed** | 사용자 공간에 버전별로 나란히 설치되는 것(예: Rust, Node, Python, JDK, 빌드 보조 CLI) | cimon, 접수 단계에서 또는 관리자 `provision` |

종류 목록과 설치 방법은 레시피 저장소 `main` 의 `toolchains.toml`(recipe-spec §16).

### 10.3 설치 규칙

- **설치는 `main` 카탈로그 항목으로만** 한다(원칙 7, §6.2). 설치 폴더와 기록은 `(kind, version, 카탈로그 항목 해시)` 로 구분한다 — 같은 버전이라도 받는 곳·검증 방법이 다르면 다른 설치다.
- **서비스 계정은 받은 것을 실행하지 않는다.**
  - `archive`·`github-release`: 서비스 계정이 내려받고 체크섬을 확인하고 `toolchains\_staging\<id>\` 에 푼다(경로 탈출 검사 포함).
  - `script`: 설치 스크립트를 **빌드 계정으로** staging 에서 실행한다. 스크립트 위치는 등록된 프로젝트 mirror 의 **전체 커밋 SHA** 로 고정한다(브랜치 이름 금지 — 그 브랜치에 push 할 수 있는 사람이 설치를 바꾸지 못하게).
  - `probe` 도 빌드 계정으로 실행한다(종료 코드 0·시간 안에 끝나야 통과). 서비스가 푼 것(`archive`·`github-release`)은 staging 에서 probe 한 뒤 최종 폴더로 옮기고 **ACL 을 다시 상속받게 한다**(옮기기는 원래 ACL 을 그대로 가져온다; 링크·정션이 있으면 거절 — `icacls /reset /T` 는 정션을 따라간다). `script` 의 출력은 서비스가 최종 폴더로 복사해(링크·정션 거부) 서비스 소유로 만든 뒤 probe 한다. 어느 쪽이든 빌드 계정에는 읽기만 남는다.
  - `rustup`·`uv-python`: 검증을 스스로 하는 bootstrap 도구라 서비스 계정이 실행한다. `uv-tool` 과 `uv-python` 의 `packages` 는 빌드 없이 휠만(`--no-build`) 설치한다 — 패키지의 빌드 스크립트가 서비스 계정으로 돌지 않게. `packages` 는 `name==version` 만 받는다(옵션·URL·경로를 uv 에 넘기지 않는다). uv 계열은 인터프리터·venv·실행기에 절대 경로를 박으므로 staging 이 아니라 최종 폴더에 바로 설치한다.
- **체크섬 출처가 없는 managed 도구는 설치하지 않는다.**
- `probe` 의 `expect` 는 정확한 버전 문자열이어야 한다(base 의 `version_any` 예외는 recipe-spec §16.1). 다르면 설치 실패.
- rustup 의 자동 설치에 기대지 않는다.

### 10.4 자동 설치 허용 — 승인 질문은 없다

- `cimon.toml [toolchains].auto_approve` 에 든 종류는 접수 단계에서 `main` 카탈로그로 설치한다.
- 나머지는 **관리자가 `cimon provision <project>`**(또는 `--kind <k> --version <v>`)로 미리 설치한다. 없으면 접수에서 `inconclusive`/`environment`: 「jdk 17.0.19+10 이 없다 — 관리자가 `cimon provision example-app` 을 실행하거나 auto_approve 에 추가」.
- **시도 브랜치의 카탈로그 항목**은 `cimon provision --recipe-ref <branch> [--kind <k>]` 로 관리자가 검토하고 설치한다. 그래야 그 항목을 쓰는 시도 실행이 돌고, 반영 조건의 「시험할 수 없는 변경」 이 시험된 것으로 바뀐다(§6.5).
- MCP 로 승인 질문(elicitation)을 보내지 않는다 — Claude Code 는 대화형 터미널에서만 그 질문을 띄우고, 서브에이전트·비대화형 호출자는 답할 길이 없다.

### 10.5 정리 · 10.6 기록

- 30일 동안 어떤 작업도 쓰지 않은 설치는 정리기가 지운다. 끝나지 않은 작업이 쓰는 설치는 지우지 않는다.
- 작업마다 실제로 쓴 툴체인(종류·버전·항목 해시·probe 출력)을 결과·`provenance.json` 에 남긴다.

### 10.7 카탈로그 밖에서 받는 것

프로젝트의 빌드 도구가 **스스로** 받는 것(빌드 시스템 wrapper 배포판, 번들러가 받는 설치 파일 제작 도구, 프로젝트 스크립트가 받는 보조 바이너리, 패키지 의존성)은 cimon 의 체크섬 보장 밖이다.

- 레시피 저장소의 프로젝트 노트에 알려진 것을 적는다.
- 작업마다 캐시·홈 디렉터리에 **새로 생긴 실행 파일**을 기록해 provenance 에 남긴다.
- 체크섬을 강제하려면 프로젝트 쪽에서 고친다(예: Gradle wrapper 의 `distributionSha256Sum`).

---

## 11. 워크스페이스

### 11.1 mirror 와 클론

- **bare mirror**: 프로젝트마다 `mirrors\<project>.git`. 접수에서 `git fetch --prune`(서비스 계정, GitHub 토큰).
- **ref 해석**: fetch 뒤 커밋으로 고정. 작업 도중 브랜치가 움직여도 그 커밋을 짓는다.
- **작업 트리는 독립 클론**이다 — mirror 를 alternates 로 참조해(`git clone --shared`) 객체는 mirror 의 것을 읽기만 하고, 인덱스·작업 트리는 클론 자신의 것이다(빌드 계정이 mirror 에 쓸 필요가 없다).
- **작업 트리의 git 은 빌드 계정으로 돈다.** clone·checkout·clean 을 러너가 단계와 같은 경로(`__exec`, Job Object)로 띄우고, 명령마다 `logs\j<id>\_workspace\` 에 로그를 남긴다. 빌드 단계는 작업 트리 전부 — `.git` 의 훅·설정·alternates 까지 — 를 쓸 수 있어서, 서비스가 거기서 git 을 돌리면 지난 작업이 심어 둔 훅이 **서비스 권한으로** 돈다. 그래서 서비스는 작업 트리에서 git 을 돌리지 않는다.
  - 사전 정리의 「추적 파일은 지우지 않는다」 규칙은 추적 파일 목록을 작업 트리 index 가 아니라 **mirror** 에서 읽는다(못 읽으면 `inconclusive`).
  - 사전 정리의 삭제는 연 핸들의 실제 경로가 작업 트리 안일 때만 **그 핸들로** 한다 — 확인 뒤 부모를 정션으로 바꿔치기해도 서비스 전용 파일로 돌려지지 않는다(읽기의 같은 규칙은 §15.1).
  - `ws\` 아래 폴더를 만들거나 지우기 전에, 그 사이 부모 경로에 링크·정션이 있으면 거절한다. 정리기가 `ws\`·`caches\` 에서 지울 때도 같다 — 프로젝트 폴더를 `artifacts\` 로 가는 정션으로 바꿔 두면 끝난 작업의 `j<id>` 를 지우는 정리가 다른 사람의 산출물을 지우게 된다.
- **git 소유자 검사(`safe.directory`)**: git 은 다른 사용자가 소유한 저장소를 열지 않는데, cimon 은 계정끼리 서로 만든 저장소를 연다 — 관리자의 `cimon recipe import` 가 만든 `recipes.git` 을 서비스가, 서비스의 mirror 를 빌드 계정(clone 원본)과 관리자(`cimon provision`)가. cimon 의 git 호출은 저장소를 **명시**하고(bare 는 `--git-dir`, 작업 트리는 빌드 계정의 `-C`) `safe.directory=*` 를 붙인다. bare 저장소는 서비스·관리자만 쓸 수 있고 위로 찾아 올라가는 탐색을 하지 않으므로, 다른 사람이 심은 저장소를 열 일이 없다 — 이 검사가 막으려는 상황이 생기지 않는다.
- **mirror 의 객체를 지키기**: 클론은 객체를 mirror 에서 빌려 쓰므로, mirror 에서 객체가 정리되면 클론이 깨진다.
  - mirror 는 `gc.auto=0`. GC 는 정리기가 **러너가 쉴 때만** 돌린다.
  - warm 슬롯·끝나지 않은 작업이 쓰는 커밋은 `refs/cimon/keep/<슬롯 또는 작업>` 으로 고정한다 — 브랜치가 지워지거나 force-push 돼도 그 커밋의 객체는 남는다.
  - warm 슬롯의 checkout·clean 이 실패하면(객체가 없거나 `.git` 이 망가졌으면) 그 슬롯을 다시 만든다(경고, 그 작업은 cold).
- 클론의 원격은 로컬 mirror 다. 빌드 계정은 GitHub 자격 증명 없이 작업 트리를 다룬다.

### 11.2 warm 슬롯

- 프로젝트마다 warm 클론을 `warm_slots` 개 둔다(기본 1 — `cimon.toml [workspace].warm_slots_per_project`, `project.toml` 의 `warm_slots` 가 덮어쓴다). 러너가 하나라 슬롯끼리 동시에 쓰이지 않는다.
- 고르는 규칙: 마지막으로 같은 **ref 이름**을 지은 슬롯 → 없으면 가장 오래 안 쓴 슬롯.
- 갱신: `git checkout --force <sha>` → `git clean -ffdx` 에 `[workspace].keep` 을 제외로.
- 슬롯이 1개면 다른 브랜치를 번갈아 지을 때마다 다시 빌드를 많이 한다. 자주 번갈아 짓는 프로젝트만 올린다.

### 11.3 setup 상태와 무효화

- warm 슬롯별로 setup 단계의 마지막 성공(커밋·`inputs` 해시·툴체인 해시·단계 정의 해시)을 `warm_state` 에 기록해 `when = "cold" | "changed"` 를 판정한다.
- **그 슬롯에서 setup 이 실패하거나, 작업이 취소·시간 초과·중단되면 그 슬롯의 `warm_state` 를 지운다.** 다음 작업은 cold 로 시작한다.
- clean 작업은 `warm_state` 를 읽지도 쓰지도 않는다.

### 11.4 clean 작업

- 일회 클론 `ws\<project>\j<id>\` 에서 처음부터, 빌드 디렉터리도 새로. 끝나면 지운다.
- **clean 이 지우지 않는 것**: `caches\<project>\`(패키지 캐시), project 범위 홈, 빌드 도구의 빌드 캐시. 결과의 `workspace` 에 「clean — 캐시 유지」 로 적는다. 빌드 캐시가 산출물·테스트 결과까지 복원하는 도구는 레시피의 `[workspace].clean_env` 로 clean 에서만 끈다.

### 11.5 형제 트리

- `project.toml` 의 `[[sibling]]` 저장소를 같은 커밋 규칙으로 클론해 넘긴다. 쓴 커밋을 결과·병합 키에 넣는다.
- 자리: `place = "isolated"`(기본) `ws\_sibling\<project>\j<id>\`, `place = "beside"` 는 **작업 트리의 부모 디렉터리** 아래 `<sibling id>`(`ws\<project>\<sibling id>\`) — 프로젝트 스크립트가 형제를 `../<이름>` 같은 상대 경로로만 찾을 때. 슬롯·clean 클론이 같은 자리를 공유하므로 작업마다 다시 맞춘다(러너가 하나라 성립).
- 넘기는 변수는 여럿일 수 있다(`env = ["A_ROOT", "A_DIR"]`).

### 11.6 자격 증명

- 서비스 계정: 등록된 저장소를 읽는 GitHub fine-grained 토큰. `cimon github-token`(표준 입력)이 DPAPI 로 암호화해 `config\github-token.dpapi` 에 두고, fetch 할 때 **`GIT_CONFIG_*` 환경 변수의 `http.https://github.com/.extraheader`** 로만 넘긴다 — 명령줄·설정 파일에 남지 않는다. **SSH 키는 두지 않는다** — git 설정 `url."https://github.com/".insteadOf "ssh://git@github.com/"`.
- 빌드 계정: **GitHub 자격 증명이 없다.** 빌드 중에 저장소를 git 으로 받는 프로젝트(cargo 의 `git` 의존성 등)를 위해 cimon 은 단계마다 `GIT_CONFIG_*` 로 git 설정을 넣는다 — 환경 변수라 레시피가 HOME 을 격리해도 적용되고, git CLI 를 부르는 도구에만 듣는다(cargo 는 `git-fetch-with-cli = true`).
  - GitHub `ssh://`·`git@github.com:` 주소는 https 로 바꾼다(SSH 키가 없다).
  - **등록된 프로젝트의 저장소는 cimon 의 mirror 에서 받는다** — 레시피 트리의 각 프로젝트 `repo`(https 와 두 ssh 표기)를 `url.<mirror>.insteadOf` 로. 자격 증명이 빌드에 들어가지 않고 네트워크를 타지 않는다. cargo 가 `rev` 로 고정한 커밋은 mirror 가 광고하지 않는 객체라 `uploadpack.allowAnySHA1InWant`, 서비스 소유 mirror 를 빌드 계정이 열도록 `safe.directory` 도 함께 둔다(빌드 계정은 mirror 에 쓸 수 없다). mirror 는 그 프로젝트를 마지막으로 받은 만큼 새롭다 — 접수가 작업의 프로젝트와 형제를 받으므로, 의존 저장소를 형제로 두면 늘 최신이다. 더 새 커밋을 원하면 git 이 mirror 경로와 함께 `not our ref` 로 실패한다.
  - 등록되지 않은 비공개 저장소만 관리자가 그 저장소 하나를 읽는 **별도 토큰**을 `cimon build-git-token <저장소 URL>`(표준 입력)로 둔다 — DPAPI 로 `config\build-git.dpapi`, 저장된 URL **에만** HTTP 헤더(`http.<url>.extraheader`)로. 모든 빌드 단계가 볼 수 있으므로 읽기 전용이어야 하고, `doctor --network` 가 접근을 확인한다.
  - git 이 아닌 도구의 자격 증명(패키지 레지스트리 로그인 등 — Conan 은 `CONAN_LOGIN_USERNAME_<REMOTE>`·`CONAN_PASSWORD_<REMOTE>` 를 읽는다)은 **빌드 비밀**이다. 관리자가 프로젝트 하나에 `cimon build-secret <project> <name>`(표준 입력)로 두면 DPAPI 로 `config\build-secrets.dpapi`, 그 프로젝트의 레시피가 **환경 변수 값에서만** `${secret.<name>}` 으로 넘긴다 — 정적 검사가 명령줄·경로·판정 인자에서는 거절하므로 명령줄·프로세스 목록에 나오지 않는다. 그 변수를 받는 모든 단계(와 그 저장소 코드)가 읽을 수 있고, 도구가 받은 세션 토큰을 홈에 남기면(`conan remote login` → `CONAN_HOME`) 그 홈을 쓰는 뒤 단계도 쓸 수 있으므로 읽기 전용이어야 한다. 단계가 쓰는 비밀이 없으면 그 단계는 `inconclusive`/`environment` 로 끝나고 `cimon build-secret` 명령을 알려 준다. `plan` 은 고른 단계에 필요한 비밀과 있는지를, `doctor` 는 `main` 레시피가 쓰는 비밀이 모두 있는지를 보인다.
  - 어느 프로젝트가 어느 저장소를 받는지는 레시피 저장소 노트.
- `core.longpaths=true`.

---

## 12. 산출물

### 12.1 왜 MCP 로 넘기지 않나

HTTP MCP 응답은 16 MiB 를 넘으면 실패한다. 1단계 대상 프로젝트의 산출물은 27 MB~270 MB 로 전부 그보다 크다. 결과에는 목록과 주소만, 파일은 HTTP 로.

### 12.2 수집 — 사전 정리

- **요청된 산출물의 glob 에 맞는 기존 파일을, 생산 단계가 돌기 전에 지운다.** 단계가 끝난 뒤 맞는 파일은 그 단계가 만든 것이다.
- 「단계 시작 뒤 수정된 파일만 인정」 은 쓰지 않는다 — 증분 빌드는 바뀌지 않은 파일을 다시 쓰지 않는다.
- 비용(다시 링크·다시 번들)은 **요청된 산출물에만** 든다.
- git 추적 파일은 지우지 않는다 — 맞으면 그 단계 `fail`/`recipe`.
- 개수가 `expect`·`expect_min` 과 다르면 그 단계 `fail`/`code`.
- 수집은 **서비스 계정**이 한다: 복사, sha256, `inspect`. 빌드 계정은 `artifacts\` 에 쓸 수 없다.

### 12.3 다운로드

- `GET https://cimon.<tailnet>.ts.net/artifacts/j<id>/<name>?t=<token>`. 토큰 = HMAC(`config\link.key`, 작업·이름·만료). 기본 1시간, **`artifacts` 도구가 부를 때마다 새로** 발급. `Range` 지원. tailnet 밖에서는 열리지 않는다.
- 보관 기간이 지나면 `410 Gone` + 「j000123 의 산출물은 2026-11-02 에 만료돼 지웠다」.

### 12.4 provenance

`artifacts\j<id>\provenance.json`: 프로젝트, ref, 커밋, 형제 커밋, 레시피 출처·커밋·sha256, 파라미터, 툴체인(종류·버전·항목 해시), 워크스페이스, 단계별 결과·시간, 산출물 sha256·검사, 경고, 카탈로그 밖에서 새로 생긴 실행 파일, 러너 이름, `trial`, `signed: false`.

### 12.5 서명

산출물은 **서명 안 된 테스트 빌드**다. 서명 키는 이 머신에 없다. 플랫폼별 서명 사실(예: Android debug 서명은 빌드 계정의 키)은 `inspect` 로 드러낸다.

---

## 13. 데이터 모델

SQLite(WAL, `synchronous=FULL`). 시각은 UTC ISO 8601.

```sql
CREATE TABLE jobs (
  id               INTEGER PRIMARY KEY,          -- j000123
  tool             TEXT NOT NULL,                -- build | test | run | scaffold
  project          TEXT NOT NULL,
  ref              TEXT,
  commit_sha       TEXT,                         -- 접수에서 확정
  siblings_json    TEXT,                         -- [{project, ref, commit}]
  recipe_source    TEXT,                         -- central | project
  recipe_ref       TEXT NOT NULL DEFAULT 'main', -- 시도 실행이면 recipe/*
  recipe_commit    TEXT,                         -- 호출 때 검증에 쓴 커밋
  recipe_sha256    TEXT,
  trial            INTEGER NOT NULL DEFAULT 0,
  request_json     TEXT NOT NULL,
  plan_json        TEXT,                         -- 선택·닫기·제외 결과(시도 실행 조건 계산에 쓴다)
  merge_key        TEXT,                         -- §8.3 — 접수 끝에 채운다
  merged_into      INTEGER REFERENCES jobs(id),
  state            TEXT NOT NULL,                -- intake | provisioning | ready | running | collecting | cancelling
                                                 -- | done | merged | cancelled | queue_timeout | interrupted
  verdict          TEXT,                         -- pass | fail | inconclusive
  failure_class    TEXT,                         -- code | recipe | request | environment | infra | unobserved
  reason_code      TEXT,                         -- 접수 실패 등의 이유 코드
  status_message   TEXT,
  summary          TEXT,
  warnings_json    TEXT,
  result_json      TEXT,
  created_by       TEXT NOT NULL,
  cancel_requested_by TEXT,
  workspace_mode   TEXT, workspace_slot INTEGER,
  fixture_state    TEXT,
  created_at TEXT NOT NULL, ready_at TEXT, started_at TEXT, finished_at TEXT,
  last_updated_at  TEXT NOT NULL,
  expires_at       TEXT NOT NULL                 -- 레코드 보관(ttlMs)
);
CREATE INDEX jobs_ready ON jobs(state, created_at);
CREATE INDEX jobs_merge ON jobs(merge_key) WHERE state IN ('ready','running');
CREATE INDEX jobs_trial ON jobs(project, recipe_ref, recipe_commit) WHERE trial = 1;

CREATE TABLE tasks (                             -- 호출 하나 = task 하나(§7.5)
  task_id TEXT PRIMARY KEY, job_id INTEGER NOT NULL REFERENCES jobs(id),
  requested_by TEXT NOT NULL, client_info TEXT, via_task INTEGER NOT NULL,
  created_at TEXT NOT NULL, last_polled_at TEXT, detached_at TEXT, detached_reason TEXT
);
CREATE INDEX tasks_job ON tasks(job_id);

CREATE TABLE steps (
  job_id INTEGER NOT NULL REFERENCES jobs(id), seq INTEGER NOT NULL, step_id TEXT NOT NULL, kind TEXT NOT NULL,
  result TEXT NOT NULL,                          -- pending | running | pass | fail | inconclusive | not_run | skipped
  failure_class TEXT, reason TEXT, exit_code INTEGER, started_at TEXT, finished_at TEXT,
  excerpt TEXT, judge_json TEXT,
  PRIMARY KEY (job_id, seq)
);

CREATE TABLE excluded (job_id INTEGER NOT NULL REFERENCES jobs(id), step_id TEXT NOT NULL, reason TEXT NOT NULL, PRIMARY KEY (job_id, step_id));

CREATE TABLE artifacts (
  job_id INTEGER NOT NULL REFERENCES jobs(id), name TEXT NOT NULL, path TEXT NOT NULL,
  bytes INTEGER NOT NULL, sha256 TEXT NOT NULL, inspect_json TEXT,
  created_at TEXT NOT NULL, expires_at TEXT NOT NULL, deleted_at TEXT,
  PRIMARY KEY (job_id, name)
);

CREATE TABLE toolchain_installs (
  kind TEXT NOT NULL, version TEXT NOT NULL, entry_sha256 TEXT NOT NULL,   -- 카탈로그 항목 해시
  root TEXT NOT NULL, checksum TEXT, source_ref TEXT,                     -- main | recipe/* (provision --recipe-ref)
  installed_by TEXT NOT NULL, installed_at TEXT NOT NULL, last_used_at TEXT NOT NULL,
  PRIMARY KEY (kind, version, entry_sha256)
);
CREATE TABLE job_toolchains (
  job_id INTEGER NOT NULL REFERENCES jobs(id), kind TEXT NOT NULL, version TEXT NOT NULL,
  entry_sha256 TEXT NOT NULL, probe TEXT, PRIMARY KEY (job_id, kind)
);

CREATE TABLE warm_slots (project TEXT NOT NULL, slot INTEGER NOT NULL, last_ref TEXT, last_commit TEXT, last_used_at TEXT, PRIMARY KEY (project, slot));
CREATE TABLE warm_state (
  project TEXT NOT NULL, slot INTEGER NOT NULL, step_id TEXT NOT NULL,
  last_ok_commit TEXT, inputs_sha256 TEXT, toolchains_hash TEXT, def_sha256 TEXT, at TEXT NOT NULL,
  PRIMARY KEY (project, slot, step_id)
);

-- 제안: 메타데이터만. head·main 은 git ref 가 원본(§6.4)
CREATE TABLE proposals (
  id              INTEGER PRIMARY KEY,           -- p0012
  branch          TEXT NOT NULL,                 -- recipe/<설명>
  title TEXT NOT NULL, description TEXT, author TEXT NOT NULL,
  status          TEXT NOT NULL,                 -- open | promoted | closed
  base_commit     TEXT NOT NULL,                 -- 브랜치를 만든 main
  created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
  promoted_commit TEXT, promoted_by TEXT, promoted_at TEXT,
  override_reason TEXT,                          -- 조건을 덮어쓴 반영의 사유
  closed_by TEXT, closed_at TEXT, close_reason TEXT
);
CREATE UNIQUE INDEX proposals_open_branch ON proposals(branch) WHERE status = 'open';

CREATE TABLE proposal_comments (
  id INTEGER PRIMARY KEY, proposal_id INTEGER NOT NULL REFERENCES proposals(id),
  author TEXT NOT NULL, body TEXT NOT NULL, head_commit TEXT NOT NULL,
  created_at TEXT NOT NULL, deleted_at TEXT
);

CREATE TABLE backups (                           -- §16.5
  kind TEXT NOT NULL,                            -- db | recipes
  at TEXT NOT NULL, path TEXT NOT NULL, offsite_ok_at TEXT, error TEXT,
  PRIMARY KEY (kind, at)
);

CREATE TABLE users (
  name TEXT PRIMARY KEY, token_sha256 TEXT NOT NULL UNIQUE,
  role TEXT NOT NULL DEFAULT 'member',           -- member | admin
  created_at TEXT NOT NULL, disabled_at TEXT
);

CREATE TABLE audit (
  id INTEGER PRIMARY KEY, at TEXT NOT NULL, user TEXT NOT NULL,
  action TEXT NOT NULL,                          -- call:<tool> | cancel | download | provision | recipe_edit | recipe_sync
                                                 -- | recipe_promote | recipe_revert | recipe_import | proposal_close | token | role
  job_id INTEGER, proposal_id INTEGER, recipe_commit TEXT, tailnet_login TEXT, detail TEXT
);
```

리뷰 화면 전용 테이블(`web_sessions`·`login_codes`)은 review-ui.md §11.

---

## 14. 설정 — `cimon.toml`

비밀을 담지 않는다(사용자 토큰은 `users` 에 해시로, 링크 키는 `config\link.key`).

```toml
[server]
bind = "127.0.0.1:8770"                            # tailscale serve 만 앞에서 받는다(§15.3)
public_url = "https://cimon.<tailnet>.ts.net"
allowed_hosts = ["cimon.<tailnet>.ts.net"]         # rmcp 기본값은 loopback 뿐. serve 가 넘기는 Host 값 — M0 에서 확인

[tailscale]
serve_target = "http://127.0.0.1:8770"
require_funnel_off = true
allow_unverified_funnel = false                    # 서비스 계정이 Tailscale 을 조회할 수 없는 머신에서만 true(§15.3)
audit_identity_headers = true

[paths]
root = 'D:\cimon'

[accounts]
service = "cimon"
build = "cimon-build"

[recipes]
store = 'D:\cimon\recipes.git'
default_branch = "main"
trial_prefix = "recipe/"
allowed_repo_prefixes = ["https://github.com/<org>/"]

[review]                                           # 반영 조건(§6.5)
require_trial = "changed_projects"                 # changed_projects | none
trial_must_pass = true

[runner]                                           # 러너는 하나(동시 실행은 §17)
name = "cimon-01"
platform = "windows-x86_64"
capabilities = []                                  # display·device·gpu 없음

[base]
git = 'C:\Program Files\Git'
android_sdk = 'C:\Android\Sdk'
inno_setup = 'C:\Program Files (x86)\Inno Setup 6'

[bootstrap]                                        # 필수 — setup 이 이 버전을 받는다(§10.1)
rustup = "1.29.2"                                  # rustup-init
uv = "0.13.5"

[toolchains]
auto_approve = ["rust", "node", "python", "cmake", "task"]

[queue]
max_queued_per_user = 5
max_queue_hours = 12

[workspace]
warm_slots_per_project = 1

[backup]
offsite = '\\<file-server>\cimon-backup'           # 필수. 외부 사본을 두지 않으려면 "none" 을 명시(§16.5)
keep = 14

[disk]
min_free_gb = 100
cache_max_gb_per_project = 50
log_max_mb_per_step = 200

[web]                                              # 리뷰 화면(M9)
enabled = false
session_hours = 12
login_code_minutes = 5
```

보관 기간은 §16.4 의 표가 정본이다.

---

## 15. 보안

### 15.1 두 계정

| 계정 | 하는 일 | 쓸 수 있는 곳 |
|---|---|---|
| `cimon`(서비스) | MCP·HTTP 서버, 작업 저장소, mirror fetch, 툴체인 다운로드·검증·배치, 레시피 저장소, 산출물 수집·서빙 | `D:\cimon` 전부 |
| `cimon-build`(빌드) | 단계 실행, 판정·검사 스크립트, **설치 스크립트와 probe 실행** | `ws\`·`caches\`·`homes\`·`tmp\`, 실행 중인 단계의 `logs\j*\*\out\`, 설치 중인 `script` 설치기의 staging 폴더. `mirrors\`·`toolchains\`(staging 포함)·`recipes\<commit>\`·`artifacts\` 는 읽기만, `state\`·`config\`·`recipes.git`·`backup\` 은 접근 불가 |

둘 다 비관리자(서비스 계정은 백업·복원 권한을 가진다 — §9.2). 작업 트리는 빌드 계정이 쓸 수 있으므로, 서비스가 거기서 읽는 파일(산출물·JUnit 보고서·fixture 증거)은 **연 핸들의 실제 경로**가 작업 트리 안인지 확인한 뒤에만 읽는다 — 정션으로 서비스 전용 파일을 산출물로 빼내지 못한다. 이렇게 나누면 브랜치의 빌드 스크립트도, 시도 브랜치의 카탈로그도, 받은 바이너리도 **작업 저장소·다른 작업의 산출물과 해시·링크 서명 키·설치된 툴체인·레시피 저장소를 바꿀 수 없다.**

### 15.2 위협과 대응

| 위협 | 대응 |
|---|---|
| 아무나 빌드를 건다 | 사용자별 Bearer 토큰, DB 에는 해시. 모든 호출을 감사 로그에 |
| 허용하지 않은 저장소·브랜치를 짓는다 | `allowed_refs`(즉시 거절), 접수에서 커밋이 허용 브랜치에서 닿는지 확인 |
| 브랜치·시도 브랜치가 권한·설치를 바꾼다 | `project.toml` 과 카탈로그 설치 항목은 `main` 의 것만(원칙 7, §6.2, §10.3). 시도 브랜치의 새 프로젝트는 `allowed_repo_prefixes` 안만 |
| 레시피를 고칠 수 있는 사람 | 레시피 = 빌드 머신 명령. 시도 브랜치는 팀원 누구나 — 토큰을 가진 팀원을 그만큼 믿는 전제. `main` 은 아무도 직접 고치지 않고, 반영·되돌리기는 관리자만. 조건을 덮어쓴 반영·되돌리기는 사유와 함께 영구히 보인다 |
| 리뷰어가 본 것과 다른 레시피가 반영된다 | `recipe_promote` 에 head 필수, 반영 순간 조건 재계산, ref 비교 후 갱신(§6.5) |
| 설치 스크립트가 서비스 계정으로 돈다 | 서비스 계정은 받은 것을 실행하지 않는다 — 설치 스크립트·probe 는 빌드 계정으로 staging 에서. 스크립트 위치는 커밋 SHA 로 고정(§10.3) |
| 빌드 스크립트가 작업 트리의 `.git`(훅·설정)을 심어 서비스 계정으로 실행시킨다 | 작업 트리의 git 은 빌드 계정으로 돈다. 서비스는 작업 트리에서 git 을 돌리지 않고, 거기서 읽거나 지우는 파일은 연 핸들의 실제 경로로 확인한다(§11.1) |
| 빌드 스크립트가 다른 프로젝트를 오염시킨다 | 캐시를 프로젝트별로. 남는 위험은 아래 「받아들이는 위험」 |
| 빌드 스크립트가 비밀을 훔친다 | 서명 키·배포 자격 증명을 두지 않는다. 빌드 계정에는 기본적으로 GitHub 자격 증명도 없다 |
| 빌드 스크립트가 네트워크로 퍼진다 | tailnet 에서 `tag:cimon` 이 출발하는 grant 없음, 기본 규칙(모두→모두) 삭제(§15.3). 빌드 계정 아웃바운드는 Windows 방화벽으로 GitHub·패키지 레지스트리·사내 아티팩트 저장소만 |
| 파라미터로 명령을 주입한다 · 저장소가 도구를 가로챈다 | 선언된 파라미터만, 모양 검증, argv 로(§9.3). 경로 구분자 없는 명령은 구성한 `PATH` 에서만 |
| 레시피 저장소가 사라진다 | 원격이 없으므로 반영마다·매일 번들 백업을 빌드 머신 밖으로(§16.5). 외부 사본이 하루를 넘게 낡으면 `recipe_*` 결과마다 경고 |
| 산출물 링크가 샌다 · 서버가 노출된다 | 링크 토큰 1시간, tailnet 주소. 서버는 `127.0.0.1`, **Funnel 금지** |
| 팀원 PC 에서 토큰이 샌다 | `headersHelper` + DPAPI(§15.4). 사용자별 토큰이라 하나만 끈다(`cimon token disable`) |
| 빌드 머신의 다른 프로세스가 `127.0.0.1:8770` 을 부른다 | 토큰이 없으면 401. Tailscale 신원 헤더는 **인증에 쓰지 않는다** |
| 리뷰 화면 공격(XSS·CSRF·세션) | review-ui §10.3 |

**두 겹**: 네트워크(Tailscale 기기 인증 + ACL)와 서버(사용자 토큰). **받아들이는 위험**: 빌드 계정은 모든 프로젝트의 작업 트리를 읽고 **쓸 수 있다** — 한 작업이 다른 브랜치·프로젝트의 warm 슬롯(`keep` 으로 남는 빌드 결과 포함)과 캐시를 바꿔 다음 작업의 산출물을 오염시킬 수 있다는 뜻이다. 작업 사이에는 빌드 계정의 프로세스·예약 작업을 모두 정리하므로(§9.2) 오염은 파일로만 남는다. 등록된 팀 내부 저장소만 짓는다는 전제로 받아들인다. 의심되면 `clean = true` 로 짓는다. 줄이려면 프로젝트별 빌드 계정이나 VM(§17).

### 15.3 Tailscale

```
팀원 PC --(tailnet, WireGuard)--> cimon.<tailnet>.ts.net:443
                                   tailscale serve (HTTPS, 인증서 자동)
                                     --> http://127.0.0.1:8770  cimon
```

- **빌드 머신**: **unattended 모드**(`tailscale up --unattended=true`, 관리자 계정으로), 태그 `tag:cimon`, 키 만료 꺼짐 확인(기본 180일마다 재인증), `tailscale serve --bg --https=443 http://127.0.0.1:8770`(재부팅 뒤에도 유지, `127.0.0.1` 대상만 프록시), tailnet 에 HTTPS 인증서 켜기. **Funnel 은 켜지 않는다** — 서비스가 시작할 때와 `doctor` 에서 `tailscale serve status --json` 을 읽어 켜져 있으면 **시작을 거부한다**. Windows 에서 Tailscale 이 사용자 계정으로 로그인돼 있으면 Tailscale 은 그 사용자에게만 답해 서비스 계정은 상태를 읽지 못한다(`401 … already in use by <사용자>`). 그런 머신은 `[tailscale].allow_unverified_funnel = true` 로 시작하되 그 사실을 로그와 `/healthz` 에 계속 싣고, Funnel 확인은 그 사용자로 돌리는 `doctor` 와 **tailnet 정책**이 맡는다 — 정책에 `funnel` 노드 속성이 없으면 Funnel 을 켤 수 없다.
- **ACL**(초안):

  ```jsonc
  {
    // 기본 규칙(모든 기기 → 모든 기기)이 있으면 지운다. 아래 grant 만 남긴다.
    "groups": {
      "group:cimon-users":  ["<팀원 계정>", "..."],
      "group:cimon-admins": ["<관리자 계정>"]
    },
    "tagOwners": { "tag:cimon": ["group:cimon-admins"] },
    "grants": [
      { "src": ["group:cimon-users"],  "dst": ["tag:cimon"], "ip": ["tcp:443"] },
      { "src": ["group:cimon-admins"], "dst": ["tag:cimon"], "ip": ["tcp:443", "tcp:3389"] }
      // tag:cimon 을 src 로 하는 grant 는 두지 않는다
    ]
  }
  ```

- **신원 헤더**: `serve` 가 붙이는 `Tailscale-User-Login` 은 **감사 로그에만**. 토큰 사용자와 다르면 경고.
- **성능**: 직접 연결이 안 되면 DERP 중계로 느려진다. `tailscale status` 로 확인, 끊기면 `Range` 로 이어받는다.
- **Tailscale 이 내려가면** 서비스에 닿을 수 없다. 받아들인다 — 관리자는 빌드 머신 콘솔로.

### 15.4 팀원 PC 의 토큰 — `headersHelper`

`claude mcp add --header` 는 토큰을 설정 파일에 평문으로 남긴다. 대신 `headersHelper`(연결할 때 실행해 헤더 JSON 을 받는 명령, 제한 10초)로 DPAPI 암호화 파일에서 읽는다.

1. 토큰 저장(한 번):
   ```powershell
   New-Item -ItemType Directory -Force "$env:USERPROFILE\.cimon" | Out-Null
   Read-Host -AsSecureString "cimon token" | ConvertFrom-SecureString |
     Set-Content "$env:USERPROFILE\.cimon\token.dpapi"
   ```
2. `%USERPROFILE%\.cimon\header.ps1`:
   ```powershell
   $secure = Get-Content "$env:USERPROFILE\.cimon\token.dpapi" | ConvertTo-SecureString
   $token = [System.Net.NetworkCredential]::new('', $secure).Password
   @{ Authorization = "Bearer $token" } | ConvertTo-Json -Compress
   ```
3. Claude Code 등록(명령 프롬프트에서, 절대 경로 — Windows 기본 실행 정책은 `.ps1` 을 막으므로 `-ExecutionPolicy Bypass`):
   ```
   claude mcp add-json --scope user cimon "{\"type\":\"http\",\"url\":\"https://cimon.<tailnet>.ts.net/mcp\",\"headersHelper\":\"powershell -NoProfile -ExecutionPolicy Bypass -File C:/Users/<me>/.cimon/header.ps1\"}"
   ```
   팀원용 전체 안내(macOS·Linux 포함)는 [remote-use.md](remote-use.md).

---

## 16. 운영

### 16.1 설치 — 처음 한 번

명령 단위의 절차는 [setup.md](setup.md). 요약:

1. `cimon setup`(관리자, `--dry-run` 으로 미리 보기): 디렉터리, `config\link.key` 생성, **Windows 서비스 등록**(sc.exe, 절대 경로, 자동 시작·실패 시 재시작 — `NT SERVICE\cimon` 은 서비스가 있어야 해석되므로 먼저), 빌드 계정 생성(PowerShell, 비밀번호는 stdin 으로, 「비밀번호 만료 없음」 을 같은 호출에서)과 DPAPI 비밀번호 파일, 권한(`NT SERVICE\cimon` 「서비스로 로그온」·토큰 바꾸기·할당량·백업·복원, 빌드 계정 「일괄 작업으로 로그온」), ACL(§5, icacls), rustup·uv bootstrap(`[bootstrap]` 버전, 체크섬 확인). **빌드 계정 아웃바운드 제한은 자동화하지 않았다** — Windows 방화벽은 사용자별 아웃바운드 규칙이 약해 수동 단계로 남기고 `setup` 이 안내한다.
2. Tailscale(§15.3).
3. 첫 관리자: `cimon token create <이름> --admin`.
4. 레시피 저장소: `cimon recipe import --init <사내 레시피 폴더> --author <이름>` — 저장소와 `main` 을 한 번 만든다(정적 검사를 통과해야 하고, `main` 이 있으면 거절). 바로 번들 백업.
5. base 툴체인 설치 → `cimon doctor` → `cimon provision <project>`(auto_approve 밖 도구).

사용자·토큰 관리: `cimon token create <이름> [--admin]`, `cimon token disable <이름>`, `cimon token rotate <이름>`, `cimon user role <이름> admin|member`.

### 16.2 머신 준비 체크리스트

| 항목 | 설정 |
|---|---|
| 사양 | 16스레드 이상 · RAM 64 GB · NVMe 2 TB |
| 디스크 | `D:` 를 Dev Drive 로 |
| 경로 길이 | `LongPathsEnabled=1`, git `core.longpaths=true` |
| Defender | Dev Drive 성능 모드, 또는 `D:\cimon` 예외 |
| Smart App Control | 끈다 — 빌드한 실행 파일의 실행을 막은 사례가 있다 |
| Windows Update | 사용 시간대 — 빌드 도중 재부팅 금지 |
| 전원 | 절전 안 함 |
| Tailscale | unattended, `tag:cimon`, 키 만료 꺼짐, `serve --bg`, Funnel 꺼짐, HTTPS 인증서 |
| 방화벽 | 서버용 인바운드 규칙 없음. 관리자 RDP 는 `100.64.0.0/10` 에서만. 빌드 계정 아웃바운드 제한 |
| base 툴체인 | 레시피 저장소 카탈로그가 요구하는 것 — `cimon doctor` 가 빠진 목록을 낸다 |
| 백업 대상 | `[backup].offsite` 에 쓸 수 있는 빌드 머신 밖 위치 |

### 16.3 팀원 PC 준비

팀원용 안내는 [remote-use.md](remote-use.md). 요약:

1. Tailscale 설치 → 회사 계정 로그인 → tailnet 관리자가 `group:cimon-users` 에 넣는다.
2. Claude Code v2.1.304 이상.
3. 관리자가 `cimon invite <이름>` 으로 **초대 코드**(Crockford base32 8자 = 40비트, 기본 10분, 한 번)를 만든다. 서버에는 코드의 해시와 이름·역할만 남는다.
4. 팀원이 서버가 내주는 스크립트를 실행한다 — `GET /connect.ps1` · `GET /connect.sh`(서버 주소가 박혀 있고 비밀은 없다). 스크립트가 `POST /connect/redeem` 에 코드를 내면 서버가 그때 사용자를 만들어 토큰을 돌려주고(한 트랜잭션에서 초대를 쓰고 사용자를 만든다), 스크립트가 토큰을 OS 보관소에 넣고 §15.4 의 헤더 도우미와 등록을 대신 한 뒤 `claude mcp get` 으로 연결을 확인한다. 이 경로들은 토큰 없이 열려 있다 — tailnet 이 문이고 코드가 자격 증명이다. 틀린 코드는 0.5초씩 늦게 답하고, 10분에 20번 넘게 틀리면 그동안 모든 교환을 거절한다. 교환·실패는 Tailscale 신원과 함께 감사 로그에.
5. 확인: `/mcp` 에 cimon 연결, `list_projects` 가 프로젝트 목록.

토큰을 직접 주는 길(`cimon token create`, §15.4 의 수동 등록)도 남는다.

퇴사·분실: tailnet 에서 기기를 빼고 `cimon token disable <이름>`.

### 16.4 관측 · 정리

- `GET /healthz`: 토큰 없이는 `{"ok": true}` 만. 사용자 토큰(`Authorization: Bearer`)을 붙이면 큐 길이, 실행 중 작업, 디스크 여유, 레시피 `main` 커밋, 마지막 백업과 외부 사본 시각, 경고.
- `cimon doctor`: base 툴체인, 두 계정과 권한(일괄 작업 로그온 포함), 디스크, GitHub 토큰(만료 14일 전 경고), 레시피 저장소(`git fsck` + `main` 정적 검사), `main` 레시피가 쓰는 빌드 비밀(§11.6), 백업, Tailscale.
- **정리 규칙 — 정본**:

| 대상 | 지우는 때 |
|---|---|
| 산출물 | 7일(`project.toml` `retention_days` 로 프로젝트별) |
| 작업 레코드·task·단계·단계 로그 | 30일(= `ttlMs`). 지운 로그는 `job_log` 가 「만료」 를 답한다 |
| 레시피 체크아웃 `recipes\<commit>\` | 끝나지 않은 작업이 안 쓰고 7일 안 쓰임 |
| 닫힌 제안의 커밋 `refs/cimon/closed/<id>` | 30일. 제안·댓글 레코드는 남긴다 |
| 열린 제안 | 30일 동안 커밋·시도 실행 없음 → 자동 닫기 |
| 툴체인 설치 | 30일 안 쓰임 |
| 캐시 `caches\<project>\` | `cache_max_gb_per_project` 초과 시 오래 안 쓴 항목부터 |
| warm 슬롯 | 디스크 하한 시 가장 오래 안 쓴 것부터 |
| 시도 브랜치에만 있던 프로젝트의 mirror·작업 폴더 | 그 프로젝트를 가리키는 열린 제안이 30일 없음 |
| 서비스 로그 | 30일 |
| 감사 로그 | 1년 |
| 백업 | `[backup].keep`(14)개 |
| 리뷰 화면 세션·로그인 코드 | 만료 후 하루 |
| 크래시 잔여물(`tmp\`, job 범위 홈, clean 클론, 형제 트리) | 서비스 시작 때 — 끝난 작업의 것만 |

- 정리기는 **러너 잠금을 잡고** 돈다(실행 중 작업의 폴더를 지우지 않는다). mirror GC 도 러너가 쉴 때만(§11.1).
- 디스크 하한에서는 위 순서대로 지운 뒤에도 모자라면 새 작업 거절(`disk_low`).

### 16.5 백업

- **레시피 저장소**(유일한 사본): `cimon recipe import --init` 직후, **반영·되돌리기마다**, 그리고 매일 `git bundle create backup\recipes-<시각>.bundle --all`. 만든 뒤 `[backup].offsite` 로 복사.
- **작업 저장소**: 매일 `VACUUM INTO 'backup\cimon-<날짜>.db'`(WAL 모드에서 파일 복사는 깨질 수 있다) → offsite 로 복사.
- `[backup].offsite` 는 **필수 설정**이다. 외부 사본을 두지 않으려면 `"none"` 을 명시해야 하고, 그러면 모든 `recipe_*` 결과에 「외부 백업 없음」 경고가 붙는다.
- **외부 사본이 26시간보다 낡으면** 모든 `recipe_*` 결과와 `/healthz` 에 경고를 싣는다 — 백업 실패가 들여다봐야만 보이는 상태로 남지 않게.
- 복구: `cimon recipe restore <bundle>` — 번들에서 `recipes.git` 을 다시 만든다. 그 뒤 제안·브랜치 맞추기(§6.5).

### 16.6 허브 업그레이드

1. **새 실행 파일로 `cimon recipe check` 를 `main` 에 돌려 통과해야** 한다 — 검사가 엄격해져 `main` 이 깨지면 모든 편집이 막힌다(§6.3).
2. 드레인: 새 작업을 거절하고(`draining`), 실행 중인 작업이 끝나면 멈춘다.
3. 교체·재시작. 레시피 변경에는 필요 없다.

---

## 17. 확장 — 1단계 뒤

| 확장 | 지금 준비된 것 | 할 일 |
|---|---|---|
| 새 프로젝트 | 레시피·프리셋·카탈로그, 시도 실행, `recipe_edit` | 레시피 저장소에 추가 |
| 레시피 초안 자동 생성 | `scaffold_recipe` 정의 | 구현(표지 파일만 읽는다, `allowed_repo_prefixes` 안만) |
| 프로젝트 레시피(`recipe_source = "project"`) | 형식·검증 시점 정의 | 구현 |
| 화면이 있는 러너 | `needs = ["display"]`, 러너 `capabilities` | 대화형 세션에서 도는 러너 에이전트 |
| 러너 여러 대 · Linux · macOS · 동시 실행 | `platforms`, `capabilities`, 작업별 홈·클론 | 원격 러너 에이전트(tailnet `tag:cimon-runner`), 빌드 계정을 러너 수만큼, 잔류 프로세스 정리를 계정별로, 형제 「beside」 자리를 러너별로 |
| 단계 병렬(한 작업 안) | `after` 로 명시된 의존 | 자원 표시(`weight = "light"`)로 가벼운 독립 단계끼리 |
| 자동 트리거 | 같은 작업 모델 | 기본 브랜치 폴링 + 커밋 상태 표시 + 실패 알림 |
| 제안·반영 알림 | 제안·감사 기록 | 메신저 webhook |
| 레시피 저장소의 외부 원격 | 평범한 git | 사내 비공개 원격으로 단방향 push |
| 프로젝트별 격리 강화 | 빌드 계정 분리 | 프로젝트별 빌드 계정 또는 VM |

---

## 18. 구현 순서

각 단계의 「끝났다」 기준에 **실패 경로를 하나 이상** 넣었다.

| 단계 | 내용 | 끝났다고 보는 기준 |
|---|---|---|
| M0 | MCP 서버 뼈대(rmcp, `server/discover`) + 인증 + `list_projects` + Tailscale `serve` | 사무실 밖 PC 에서 tailnet 주소로 도구 목록이 보인다. tailnet 밖에서는 안 된다. 토큰 없이 401. Funnel 을 켜면 시작 거부 |
| M1 | 레시피 저장소(`import --init`·`export`·`show`) + `cimon-recipe` 정적 검사 + `recipe check` + `describe_project`·`plan`(정적) + **레시피 번들 백업·offsite** | `import --init` 이 `main` 을 만들고 번들이 offsite 에 생긴다. 두 번째 `--init` 은 거절. 검사에 실패한 폴더는 들여오지 않는다. 로컬 `recipe check` 가 접수 검사를 「건너뜀」 으로 명시한다 |
| M2 | 작업 저장소 + 접수/대기/실행 + 두 계정 + Job Object + task 대응(`build`·`test`·`run`) + **mirror·독립 클론(warm 1·clean)** + 시도 실행(`recipe_ref`) + 2차 병합 + 작업 저장소 백업 | 5분을 넘는 작업이 끝까지 간다. push 뒤 다시 요청하면 새 커밋으로 돈다(옛 결과를 받지 않는다). 하나를 `tasks/cancel` 해도 다른 task 는 결과를 받는다. 빌드 단계가 `cimon.db`·`artifacts\`·`toolchains\` 에 쓰지 못한다. 예약 작업 프로세스가 작업 뒤에 남지 않는다. task 를 못 쓰는 호출자(서브에이전트·`claude -p`)가 `accepted` 결과를 스키마 오류 없이 받는다 |
| M3 | 툴체인 관리자(`main` 카탈로그, 항목 해시, staging, 빌드 계정 실행) + `cimon provision`(`--recipe-ref` 포함) | 빈 머신에서 모든 프로젝트 provision 이 끝난다. 체크섬이 틀리면 설치 안 됨. 시도 브랜치에서 카탈로그를 바꿔도 접수가 설치하지 않고 `environment` 로 알린다. 설치 스크립트가 `state\` 에 쓰지 못한다 |
| M4 | 산출물(사전 정리·수집·검사·HTTP) | 받은 파일 sha256 이 결과와 같다. 옛 버전이 쌓인 폴더에서도 새 것 하나만. `required` 검사 실패가 `inconclusive`/`recipe` |
| M5 | 레시피 저장소의 모든 프로젝트 build·test(형제, warm 슬롯, setup 무효화, mirror 객체 보호, 계정 환경은 M7) | 기본 build·test 가 `pass`. 깨뜨린 브랜치는 `fail`/`code` 와 첫 에러 줄. setup 을 취소한 뒤 다음 작업이 cold. 지운 브랜치를 마지막으로 지은 슬롯이 GC 뒤에도 깨지지 않는다. 노트의 음성 대조 시험이 실패한다 |
| M6 | 레시피 편집·제안·반영(§6.4~6.5): 여러 파일 커밋, 정적 검사 범위, 의미 차이·위험 표시, 반영 조건(시도 실행 범위), `recipe_*` 도구 전부, 역할, 쓰기 잠금, 시작 맞추기 | 여러 파일 변경(단계 이름 바꾸기)이 한 번에 저장된다. `main` 편집은 `main_not_editable`. 바뀐 단계를 안 돌린 시도 실행으로는 조건이 안 채워진다. 낡은 head 로 반영하면 거절. 사유 없이 덮어쓰기 거절. 반영 직후 서비스를 죽여도 재시작 뒤 제안이 `promoted` |
| M7 | fixture + `script` 판정기 + 계정 환경 + 시나리오 | 시나리오가 판정을 낸다. 관측 창을 줄이면 `inconclusive`/`unobserved`. 브랜치가 지은 CLI 로 기동이 실패하면 `fail`/`code`. 취소해도 스택이 남지 않는다. 예약 작업으로 뜬 스택이 작업 홈을 본다 |
| M8 | 운영 — 정리 규칙 표, 드레인·업그레이드 전 검사, doctor, 감사, 복구 | 7일 지난 산출물이 `410`. 번들로 `restore` 하면 그 시점 `main`. 디스크를 채우면 정리기가 먼저 돈다. offsite 가 26시간 낡으면 `recipe_*` 결과에 경고 |
| M9 | 리뷰 화면 + `web_login`(review-ui) | review-ui §14 |

---

## 19. 미결정 · 결정

### 19.1 확인 필요(서비스 쪽)

| 항목 | 언제 | 메모 |
|---|---|---|
| 빌드 계정으로 자식 프로세스를 띄울 때 서비스 계정에 필요한 권한 | M2 | `CreateProcessAsUser` |
| `serve` 가 넘기는 Host 헤더 값 | M0 | `allowed_hosts` |
| 레시피 저장소 git 구현 | M1 | `git2` 크레이트 또는 git CLI |
| 빌드 계정 아웃바운드 허용 목록 | M2 | 레시피 저장소 노트에서 모은다 |
| 작업 스케줄러가 새 프로세스에 계정 사용자 환경을 넣는지 | M7 | §9.2 |
| 백업 offsite 위치 | 팀 | 레시피 저장소의 유일한 외부 사본 |
| tailnet 관리자·요금제·이름, 기본 규칙 유무, 사무실 UDP | 팀 | §15.3 |
| 빌드 머신 확보 · Visual Studio 라이선스 | 팀 | §16.2 |

프로젝트별 확인 항목은 레시피 저장소의 프로젝트 노트에 있다.

### 19.2 결정 기록

| 날짜 | 결정 | 이유 |
|---|---|---|
| 2026-10-25 | Windows 1대, 실행 엔진은 직접 구현 | 러너 1대·하나씩이라 감당할 만하고, 에이전트 인터페이스와 「실패가 보이게」 하는 판정은 어차피 직접 만든다 |
| 2026-10-25 | 빌드·테스트 방법은 레시피(데이터)로 | 새 프로젝트·테스트 수정에 코드 변경·재배포가 없게 |
| 2026-10-25 | 접속 경로는 Tailscale 하나, Funnel 금지 | 외부에서도 쓰되 포트를 인터넷에 열지 않는다 |
| 2026-10-26 | 이름 cimon | zenmon 형제 이름, 「Simon says」 |
| 2026-10-26 | `run` 도구, `build`·`test` 는 그 줄임 | 「빌드해서 받고 테스트도」 를 호출 한 번으로 |
| 2026-10-26 | 접수와 실행을 나눈다. 접수 작업자 1, 러너 1 | 러너가 놀지 않고, 커밋 기준 병합을 러너 전에. 1단계에 동시성은 이득보다 경합이 크다 |
| 2026-10-26 | 중복 병합은 접수 끝에서 한 번, 키에 형제 커밋까지 | 호출 시점 병합은 push 직후 요청에 옛 결과를 준다 |
| 2026-10-26 | task 와 작업은 다대일, 취소는 task 단위로 뗀다 | 병합이 다른 사람의 작업을 대신 취소하지 않게 |
| 2026-10-26 | 시작 전 작업은 유지, 상한은 12시간·사용자당 5개 | 결과는 서버가 갖는다. 「아무도 안 보면 버리기」 는 뺐다 |
| 2026-10-26 | 서비스 저장소와 레시피 저장소를 나눈다 | 원격 빌드 서비스는 프로젝트를 몰라야 한다 |
| 2026-10-26 | 레시피 저장소는 빌드 머신 안의 git, 편집은 MCP 도구 | 저장소를 하나 더 관리하지 않는다. 대가: 번들 백업이 필수이고 외부 사본은 필수 설정(§16.5) |
| 2026-10-26 | 시도 브랜치는 팀원 누구나, `main` 은 아무도 직접 고치지 않는다. 반영은 관리자, fast-forward 만, head 고정 | 레시피 = 빌드 머신 명령. 모든 변경이 조건을 거치고, 시험하지 않은 병합 조합이 들어가지 않게 |
| 2026-10-26 | `recipe_edit` 은 여러 파일을 커밋 하나로, 이번 변경이 새로 만든 오류만 저장을 막는다 | 단계 이름 바꾸기처럼 여러 파일에 걸친 변경이 중간 상태로 막히지 않게, 다른 프로젝트의 문제가 편집을 막지 않게 |
| 2026-10-26 | 검사는 정적(레시피만)과 접수(그 커밋·서버 설정)로 나눈다 | 로컬 `recipe check` 가 할 수 없는 검사를 통과로 보이지 않게 — 「건너뜀」 으로 명시 |
| 2026-10-26 | 반영 조건의 시도 실행은 바뀐 단계를 포함한 최신 실행만. 시험할 수 없는 변경은 사유 필수 | 변경과 무관한 실행이나 재시도로 조건을 채우지 못하게 |
| 2026-10-26 | 반영 조건·제안은 M6(MCP), 리뷰 화면은 M9 | 화면을 마지막에 만들어도 조건이 비지 않게 |
| 2026-10-26 | 툴체인은 `main` 카탈로그로만 설치, 서비스 계정은 받은 것을 실행하지 않는다 | 시도 브랜치·외부 브랜치가 서비스 계정에 코드를 실행시키지 못하게 |
| 2026-10-26 | 한 작업 안의 단계는 차례로, warm 슬롯은 프로젝트당 기본 1 | 1단계 단순함 |
| 2026-10-26 | fixture 기동 실패의 분류는 레시피가 정한다(`on_up_failure`) | 브랜치가 지은 실행 파일로 띄우는 스택의 기동 실패가 「인프라, 재시도」 로 보이지 않게 |
| 2026-10-26 | `exit` 판정기를 쓰는 테스트는 `uncounted` 로 밝힌다 | 0개 실행을 못 잡는 단계가 있다는 사실이 레시피와 결과에 보이게 |
| 2026-10-26 | 프로젝트별 결과 판정은 `script` 판정기로 레시피 저장소에 | 특정 프로젝트의 결과 형식을 아는 코드를 서비스에 넣지 않는다 |
| 2026-10-26 | 빌드 계정 분리 | 빌드가 서비스 상태·다른 작업의 산출물을 바꾸지 못하게 |
| 2026-10-26 | 실패 분류와 허용 조합, 작업 분류는 최악 판정의 첫 단계 | 「다시 돌릴까」 와 「코드를 고칠까」 를 추측하지 않게 |
| 2026-10-26 | 승인 질문 대신 관리자 `cimon provision` | 비대화형 호출자가 답할 수 없고, 설치는 드물다 |
| 2026-10-26 | `scaffold_recipe`·프로젝트 레시피는 형식만, 구현은 2단계 | 1단계에 쓰는 곳이 없다 |
| 2026-10-26 | 리뷰 화면은 보기·토론·반영 전용, 의미 차이가 중심. 줄 댓글·좌우 보기·구문 강조는 뺀다. 처음의 「PR 화면 없음 — `recipe_diff` 로 대신」 을 대체한다 | 편집은 한 곳에서. 여러 프로젝트에 걸친 변경을 사람이 훑어보기에는 화면이 낫다. 1단계 규모에 맞게 |
| 2026-10-26 | 리뷰 화면 로그인은 MCP 가 발급하는 일회용 링크 | 브라우저에 Bearer 토큰을 넣지 않는다. 신원 헤더는 로컬 프로세스가 꾸밀 수 있다 |

---

## 부록 A. 근거(외부, 2026-10-25~26 확인)

- Claude Code MCP 문서 — tasks, 요청별 타이머, 출력 한도, 인증, `headersHelper`: https://code.claude.com/docs/en/mcp
- Claude Agent SDK custom tools — `resource_link`, `structuredContent`: https://code.claude.com/docs/en/agent-sdk/custom-tools
- MCP 2026-07-28 changelog: https://modelcontextprotocol.io/specification/2026-07-28/changelog
- MCP Tasks 확장: https://modelcontextprotocol.io/extensions/tasks/overview, https://github.com/modelcontextprotocol/ext-tasks
- rmcp: https://github.com/modelcontextprotocol/rust-sdk(태그 `rmcp-v3.7.0`, `examples/servers/src/common/task_demo.rs`)
- Tailscale Serve: https://tailscale.com/kb/1312/serve, CLI: https://tailscale.com/kb/1242/tailscale-serve, unattended: https://tailscale.com/kb/1088/run-unattended
