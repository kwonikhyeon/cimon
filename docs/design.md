# cimon 설계

> **Cimon says: build.**

- 이름: **cimon**(「시몬」 으로 읽는다). CI + mon — zenmon(zenoh + monitor)의 형제 이름이다. 「Simon says」(시키는 대로 하는 놀이)처럼, 사람이 Claude 를 통해 시키면 빌드하고 결과를 가져온다. 같은 이름의 PLC·SCADA 브랜드(대문자 CIMON)와 헷갈리지 않도록 소문자로 쓴다.
- 상태: 초안 · 2026-10-26 — 구현 전
- 「확인 필요」 는 아직 실측·실행으로 확인하지 않은 가정이다.

| 문서 | 내용 |
|---|---|
| **design.md**(이 문서) | 서비스 구조 · MCP 인터페이스 · 작업 모델 · 실행 · 저장 · 보안 · 운영 |
| [recipe-spec.md](recipe-spec.md) | 레시피 파일 형식(정본) |
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
| Windows 빌드 머신 1대 | Linux·macOS·ARM — 플랫폼마다 네이티브 머신이 필요하다(§17) |
| 레시피 저장소에 등록된 프로젝트(1단계는 3개) | GPU·제조사 SDK 가 필요한 프로젝트 |
| 빌드 · 테스트 · 패키징 · 시뮬레이터 위 시나리오 시험 | 실장비 시험(HIL) |
| 서명 없는 테스트 산출물 다운로드 | 서명·릴리스 발행 — 서명 키를 이 머신에 두지 않는다(§15) |
| 화면이 필요 없는 테스트 | 화면에 그리는 테스트 — 서비스는 데스크톱 없는 세션 0 에서 돈다. 레시피가 `needs = ["display"]` 로 표시하면 **제외되고 제외 사실이 결과에 보인다** |
| 팀 tailnet(Tailscale) 안에서만 | 공개 인터넷 노출(Tailscale Funnel 포함) |
| 작업 하나씩(러너 동시 1) | 빌드 동시 실행 |

---

## 2. 설계 원칙

1. **서비스는 프로젝트를 모른다.** 프로젝트에 관한 모든 것은 레시피 저장소의 데이터다. 서비스 코드에 특정 프로젝트·특정 도구를 위한 분기를 두지 않는다.
2. **프로젝트의 빌드를 다시 구현하지 않는다.** 프로젝트 자신의 진입점(빌드 CLI, `task`, `gradlew`, 스크립트 …)을 부른다.
3. **선언한 것만 쓴다.** 레시피에 없는 툴체인으로 대신 짓지 않는다. 툴체인 버전은 정확히 고정한다. 서비스 프로세스의 `PATH` 를 자식에게 물려주지 않는다.
4. **실패는 실패로 보인다.** 판정은 통과·실패·판정 불가 셋이다. 건너뛴 단계, 빠진 산출물, 0개 실행된 테스트, 아무것도 돌지 않은 작업은 통과가 아니다. 제외한 것은 결과 맨 앞에 적는다. 실패가 코드 탓인지 환경 탓인지 가른다.
5. **결과는 서버가 갖는다.** Claude Code 는 세션을 끝내면 task 를 더 따라가지 않는다. 작업 ID 만 있으면 다른 세션·다른 날에도 결과와 산출물을 찾을 수 있어야 한다.
6. **확장은 데이터로.** 새 프로젝트·새 테스트·새 도구 종류는 레시피와 카탈로그로 표현한다. 코드가 바뀌는 것은 새 「방식」(판정기·설치 방식·러너 능력)이 필요할 때뿐이다.
7. **권한에 닿는 설정은 레시피 저장소의 기본 브랜치에만.** 저장소 주소·허용 ref·형제 저장소 접근은 `project.toml` 에 두고, 이미 등록된 프로젝트의 `project.toml` 은 언제나 기본 브랜치에서 읽는다. 빌드하는 브랜치도, 시도 실행 브랜치도 권한을 넓히지 못한다.
8. **관리자 권한이 드는 일은 사람이 한다.** MCP 호출 하나로 시스템 전역 설치가 일어나지 않는다.
9. **빌드는 임의 코드 실행이다.** 빌드는 서비스와 다른 계정으로 돌고, 그 계정은 서비스의 상태를 건드리지 못한다.

---

## 3. 세 장면

예시 프로젝트 `example-app`(Rust 백엔드 + Gradle 로 만드는 Android 앱)으로 설명한다.

**장면 1 — 빌드하고 테스트하고 산출물 받기**

> 「example-app 의 Android APK 빌드해서 받고, 공유 모듈 테스트도 돌려 줘」

1. Claude → `run(project="example-app", steps=["build-apk", "test-shared"], artifacts=["app-debug.apk"])`
2. cimon: 입력을 레시피로 검증(모르는 단계·파라미터면 여기서 거절) → 작업을 DB 에 커밋 → **바로** task 응답.
3. 접수 단계(러너 밖): fetch → 커밋 고정 → 계획 → 툴체인 확인·설치. `/tasks` 에 「접수: git fetch」 → 「대기열 1번째」.
4. 실행 단계(러너): 「build-apk (1/2) · 3분 10초」 → 「test-shared (2/2)」.
5. 끝: 판정 `pass`, 산출물 `app-debug.apk`(sha256, 서명 인증서 지문, 다운로드 주소).
6. Claude 가 `Invoke-WebRequest` 로 받고 sha256 을 대조한다.

**장면 2 — 테스트 고치기**

> 「example-app 에 공유 모듈 desktopTest 만 도는 테스트를 추가하고 돌려 봐」

1. Claude → `recipe_show(path="projects/example-app/recipe.toml")` 로 지금 레시피를 읽는다.
2. Claude → `recipe_edit(branch="recipe/add-desktop-test", path=…, content=…, base=<읽은 커밋>, message="desktopTest 단계 추가")`. cimon 은 **저장 전에 레시피 검사**를 돌리고, 통과하면 그 브랜치에 커밋한다(작성자 = 토큰 사용자). 깨진 레시피는 저장되지 않는다.
3. `test(project="example-app", recipe_ref="recipe/add-desktop-test", steps=["test-shared-desktop"])` → `trial: true` 로 돈다.
4. 통과하면 관리자가 `recipe_promote(branch="recipe/add-desktop-test")` — `main` 에 반영, 다음 작업부터 모두에게. 서비스 재시작 없음.

**장면 3 — 새 프로젝트**

1. 관리자가 GitHub 토큰에 저장소를 추가.
2. Claude 가 프로젝트 저장소를 읽어 초안을 쓰고, `recipe_edit` 로 `recipe/add-<id>` 브랜치에 `project.toml`·`recipe.toml` 을 만든다 → `plan` → `build`·`test` 로 시도 → 관리자가 `recipe_promote`.

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
                      GET /artifacts  |   MCP endpoint        Artifact HTTP            |
                                      |   Job store (SQLite)  Intake workers (2)       |
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
| 레시피 저장소 | 빌드 머신 안의 git 저장소. 읽기(검증·겹치기·선택), 편집 도구(검사 후 커밋), 반영·되돌리기, 가져오기·내보내기 | `recipes.git`, 메모리 캐시(커밋별) |
| 작업 저장소 | 작업·task·단계·산출물·툴체인 사용·warm 상태·감사 로그 | `state\cimon.db`(SQLite, WAL) |
| 접수 작업자 | fetch → 커밋 고정 → 레시피 → 계획 → 툴체인. 러너를 쓰지 않는다(동시 2) | 작업 저장소 |
| 러너 | 워크스페이스 → 단계 → 수집. **빌드 계정으로** 자식 프로세스를 띄운다(동시 1) | `logs\`, `homes\`, `tmp\` |
| 툴체인 관리자 | 카탈로그대로 managed 설치(서비스 계정), base 검사 | `toolchains\` |
| 워크스페이스 관리자 | bare mirror, warm 슬롯·clean 트리(독립 클론), 형제 트리 | `mirrors\`, `ws\` |
| 정리기 | 만료 산출물·로그·홈 삭제, 캐시·warm 트리 크기 관리, 디스크 하한 감시, 빌드 계정 잔류 프로세스 정리 | — |

### 4.1 코드와 레시피

| | 내용 | 어디에 |
|---|---|---|
| `cimon` 저장소 | 서비스 코드, 이 설계, 레시피 형식 명세, 형식 예시 | GitHub. 프로젝트 정보가 없으므로 공개해도 된다 |
| 레시피 저장소 | `toolchains.toml`, `presets/`, `projects/<id>/`, 판정·검사 스크립트, 프로젝트별 노트, 레시피 안내서 | **빌드 머신 안** `D:\cimon\recipes.git`. 외부에 올리지 않는다. 매일 백업(§16.4) |

레시피 저장소는 평범한 git 저장소다 — 커밋으로 버전을 고정하고, 브랜치로 시도 실행을 하고, 이력으로 누가 무엇을 바꿨는지 남긴다. 다만 GitHub 같은 원격이 없고, 사람은 cimon 의 MCP 도구로 읽고 고친다(§6.4). 처음 내용은 사내 레시피 폴더에서 `cimon recipe import` 로 들여온다.

### 4.2 코드 구조

Rust 워크스페이스. 실행 파일 하나(`cimon.exe`)가 서비스와 관리 명령을 겸한다.

```
cimon/
  Cargo.toml                 워크스페이스
  crates/
    cimon-recipe/            레시피 파싱·검증·겹치기·선택. 순수 — 네트워크·프로세스 없음
    cimon-recipestore/       빌드 머신 안 레시피 git 저장소: 읽기·편집 커밋·반영·되돌리기·가져오기·내보내기
    cimon-store/             SQLite 작업 저장소(rusqlite), 스키마 마이그레이션
    cimon-toolchain/         카탈로그, installer 구현(archive·github-release·rustup·uv·script·base)
    cimon-workspace/         git mirror·독립 클론·warm 슬롯
    cimon-runner/            빌드 계정 토큰, Job Object, 환경 구성, 단계 실행, fixture, 사전 정리, 수집
    cimon-judge/             판정기(exit·cargo-test·junit·script), 검사(apk-signer·pe-version·script)
    cimon-mcp/               rmcp 서버 처리기, 도구 정의, task↔작업 대응
    cimon/                   바이너리: serve · doctor · setup · token · provision · recipe check/show/import/export
  examples/recipes/          형식 예시(레시피 검사기의 시험 입력)
  docs/
```

`cimon-recipe` 를 순수하게 두는 이유: `recipe_edit` 이 저장 전에 돌리는 검사와, 팀원이 자기 PC 에서 내보낸 레시피에 돌리는 `cimon recipe check <경로>` 가 같은 코드로 같은 결론을 내야 한다.

---

## 5. 디스크 레이아웃

경로 길이 260자 함정을 피하려고 루트를 짧게 잡는다. Dev Drive(ReFS)에 둔다. 「서비스」 는 서비스 계정 `cimon`, 「빌드」 는 빌드 계정 `cimon-build` 의 권한이다(§15.1).

```
D:\cimon\                          서비스      빌드
  state\cimon.db                   rw          -        작업 저장소
  config\cimon.toml                rw          -        설정
  config\link.key                  rw          -        산출물 링크 서명 키
  mirrors\<project>.git            rw          r        bare mirror(독립 클론의 alternates)
  recipes.git                      rw          -        레시피 저장소(bare git) — 원본
  recipes\<commit>\                rw          r        고정한 커밋의 레시피 체크아웃(${recipes} — 판정·검사 스크립트)
  backup\                          rw          -        작업 저장소·레시피 저장소 백업(§16.4)
  ws\<project>\warm-<n>\           rw          rw       증분 빌드용 독립 클론(프로젝트당 warm_slots 개)
  ws\<project>\j<id>\              rw          rw       clean 작업용 일회 클론
  ws\<project>\<sibling>\          rw          rw       「beside」 형제 트리(§11)
  toolchains\<kind>\<version>\     rw          r        managed 툴체인
  toolchains\_bootstrap\           rw          r        rustup·uv
  caches\<project>\                rw          rw       프로젝트별 캐시 — cargo·npm·uv·gradle·nuget
  homes\p\<project>\<NAME>\        rw          rw       project 범위 홈
  homes\j<id>\<NAME>\              rw          rw       job 범위 홈
  artifacts\j<id>\                 rw          r        수집한 산출물 + provenance.json
  logs\j<id>\<step>\               rw          rw*      단계 로그(서비스가 씀) + 단계 출력 디렉터리(${step.out}, 빌드가 씀)
  logs\cimon\                      rw          -        서비스 로그
  tmp\j<id>\                       rw          rw       작업별 TEMP/TMP
```

`*` `logs\j<id>\<step>\` 안에서 빌드 계정이 쓸 수 있는 것은 `out\` 하위뿐이다. 로그 파일 자체는 서비스가 쓴다.

**디스크 예산**: 1단계 대상 프로젝트 중 가장 큰 것은 warm 트리 하나가 약 40 GB, 산출물은 작업당 수백 MB 다(프로젝트별 실측은 레시피 저장소 노트). **NVMe 2 TB 를 권장**한다. clean 작업은 warm 트리 하나만큼을 잠깐 더 쓴다.

---

## 6. 레시피

형식의 정본은 [recipe-spec.md](recipe-spec.md). 여기서는 서비스가 레시피를 어떻게 다루는지만 적는다.

### 6.1 모델 요약

| 개념 | 뜻 | 명세 |
|---|---|---|
| `project.toml` | 저장소·허용 ref·형제·레시피 출처. 레시피 저장소에만 | §3 |
| 단계(`[[step]]`) | 실행 단위. 종류 `setup`·`build`·`test`·`package`·`scenario` | §9 |
| 선택 | `build`·`test`·`run` 이 id·태그·산출물 이름·기본값으로 고르고, `after` 로 닫는다 | §14 |
| 능력(`needs`) | `display`·`device`·`gpu`. 러너에 없으면 제외하고 결과에 적는다 | §9, §14 |
| fixture | 여러 단계가 공유하는 떠 있어야 하는 것(시뮬레이터 스택 등). 올리기·준비 대기·**반드시** 내리기·증거 보관 | §11 |
| 판정기(`judge`) | `exit`·`cargo-test`·`junit`·`script`(레시피가 제공하는 판정 스크립트) | §12 |
| 파라미터 | 호출자가 고를 수 있는 값. 선언한 모양만 받는다 | §8 |
| 프리셋 | 스택별 공통 단계 | §17 |
| 툴체인 카탈로그 | 도구 종류별 설치 방식·체크섬 출처·검증 | §16 |

### 6.2 어디서 읽나

- **중앙 레시피**(`recipe_source = "central"`, 기본): 빌드 머신 안 레시피 저장소(`recipes.git`)의 `main` 에서 읽는다. 같은 머신의 git 이라 fetch 도, 묵을 일도 없다. `main` 이 움직이면(`recipe_promote`) 다음 작업부터 반영된다.
- **프로젝트 레시피**(`recipe_source = "project"`): 빌드하는 ref 의 `.cimon/recipe.toml`. **형식은 정의하되 구현은 2단계**다 — 1단계에는 쓰는 프로젝트가 없다.
- **시도 실행**: `recipe_ref` 로 레시피 저장소의 `recipe/*` 브랜치를 주면 그 브랜치의 `recipe.toml`·include·프리셋·카탈로그·스크립트로 돈다. 결과에 `trial: true`. **이미 등록된 프로젝트의 `project.toml` 은 시도 실행에서도 `main` 의 것을 쓴다**(원칙 7). 시도 브랜치에만 있는 새 프로젝트는 `repo` 가 `cimon.toml [recipes].allowed_repo_prefixes` 에 맞을 때만 받는다.
- 작업은 레시피 커밋·sha256 을 고정하고 결과에 적는다.

### 6.3 언제 검증하나

| 출처 | 입력 검증 시점 | 이유 |
|---|---|---|
| 중앙 | **task 를 만들기 전** — `main`(또는 `recipe_ref` 브랜치)의 현재 커밋으로. **검증에 쓴 레시피 커밋을 작업에 고정**한다 | 모르는 단계·파라미터는 즉시 거절해야 한다. 같은 머신의 git 이라 즉시 읽힌다. 고정하지 않으면 대기하는 동안 단계 이름이 바뀐 레시피로 돌게 된다 |
| 프로젝트(2단계) | task 를 만든 뒤 **접수 단계에서** — 프로젝트를 fetch 하고 `git show` 로 읽어서. 러너를 잡기 전이다 | 큰 저장소 fetch 를 응답 전에 하지 않는다 |

`recipe_edit` 이 저장 전에 검사하므로 `main` 의 레시피가 깨질 일은 드물다. 그래도 깨져 있으면(가져오기·되돌리기 등) 그 프로젝트의 작업만 거절되고, 다른 프로젝트는 영향받지 않는다. 거절 메시지에 파일·줄·키가 담긴다.

### 6.4 레시피 편집 — MCP 도구

레시피 저장소는 외부 원격이 없으므로, 읽고 고치는 길은 cimon 의 도구뿐이다.

| 도구 | 하는 일 | 권한 |
|---|---|---|
| `recipe_show` | 파일 하나(또는 디렉터리 목록)를 브랜치·커밋 기준으로 읽는다. 겹친 결과는 `describe_project` | 팀원 |
| `recipe_edit` | 파일 하나를 만들거나 바꾸거나 지운다. **저장 전에 그 브랜치 전체에 레시피 검사**를 돌려 통과할 때만 커밋한다 | `recipe/*` 브랜치: 팀원. `main`: 관리자 |
| `recipe_diff` | 두 브랜치·커밋의 차이 | 팀원 |
| `recipe_history` | 파일·브랜치의 커밋 이력(작성자·시각·메시지) | 팀원 |
| `recipe_promote` | `recipe/*` 브랜치를 `main` 에 반영(fast-forward, 안 되면 거절하고 다시 맞추라고 알린다). 반영 전에 `main` 기준으로 다시 검사 | 관리자 |
| `recipe_revert` | `main` 의 커밋 하나를 되돌리는 커밋을 만든다 | 관리자 |

규칙:

- **커밋 작성자는 토큰 사용자**다. 메시지는 필수. 감사 로그에도 남는다.
- **동시 편집**: `recipe_edit` 은 `base`(편집자가 읽은 커밋)를 받는다. 그 브랜치가 그 사이 움직였으면 `recipe_conflict` 로 거절하고 최신 커밋을 알려 준다. 덮어쓰지 않는다.
- **검사에 실패하면 저장하지 않는다.** 오류를 파일·줄·키로 돌려준다. 깨진 레시피가 브랜치에 들어가지 않는다.
- 시도 브랜치는 `recipe/<설명>`. 반영되거나 30일 동안 안 쓰이면 정리기가 지운다(이력은 `main` 에 남은 것만).
- 신뢰 수준: 시도 브랜치를 고칠 수 있는 사람은 시도 실행으로 빌드 머신에서 명령을 돌릴 수 있다 — 팀원 전체에게 그 권한이 있다는 전제다(§15.2). `main` 반영은 관리자가 diff 를 보고 한다.

사람이 편집기로 고치고 싶으면:

- `cimon recipe export <폴더> [--branch <b>]` — 레시피를 폴더로 꺼낸다(빌드 머신의 관리자 명령, 또는 팀원이 `recipe_show` 로 받아 로컬에 둔다).
- 로컬에서 `cimon recipe check <폴더>`.
- `cimon recipe import <폴더> --branch recipe/<설명> --message "…"` — 폴더 내용을 그 브랜치의 새 커밋으로(검사 후). 관리자 명령.

처음 내용도 `cimon recipe import` 로 들여온다.

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
| `describe_project` | | 겹친 레시피 요약 — 단계(id·종류·태그·기본·needs·설명), 산출물, 파라미터, fixture, 툴체인 |
| `plan` | | `build`·`test`·`run` 이 무엇을 할지 **실행 없이**: 돌 단계 순서, 제외와 이유, 갖출 툴체인(관리자 설치 필요 여부), 레시피 오류 |
| `build` | **●** | kind `build`·`package` 단계 |
| `test` | **●** | kind `test`·`scenario` 단계(필요한 빌드는 따라온다) |
| `run` | **●** | 종류를 가리지 않고 고른 단계를 한 작업에서 |
| `job_status` | | 작업의 상태·판정·요약(세션이 바뀐 뒤 조회용) |
| `job_log` | | 단계 로그의 끝 N 줄 또는 패턴 검색 |
| `artifacts` | | 산출물 목록 + **새로 발급한** 다운로드 링크 |
| `cancel_job` | | 작업 취소 — 그 작업을 따르는 모든 요청에서(§7.5) |
| `queue` | | 접수·대기·실행 중 작업 |
| `recipe_show` · `recipe_edit` · `recipe_diff` · `recipe_history` · `recipe_promote` · `recipe_revert` | | 레시피 저장소 읽기·편집(§6.4) |
| `scaffold_recipe` | **●** | 저장소를 읽어 레시피 초안을 만든다(등록하지 않는다, 저장소 코드를 실행하지 않는다). **2단계** |

`run` 이 기본형이고 `build`·`test` 는 고를 수 있는 종류를 좁힌 줄임이다. 툴체인 설치는 MCP 도구가 아니라 관리자 CLI `cimon provision` 이다(§10.4).

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
  "clean": false,
  "unwatched": "keep"
}
```

| 필드 | 기본 | 뜻 |
|---|---|---|
| `project` | 필수 | `list_projects` 의 id |
| `ref` | 프로젝트 `default_ref` | 브랜치 이름 또는 커밋 SHA. 허용 목록 밖이면 거절 |
| `recipe_ref` | `main` | 레시피 저장소의 `recipe/*` 브랜치 — 시도 실행(§6.2) |
| `steps` · `tags` | 아래 표 | 둘을 함께 주면 **합집합**(recipe-spec §14) |
| `artifacts` | 선택된 단계의 `default = true` 산출물 | 이름 목록. `[]` 면 수집 안 함. 이름을 대면 그 생산 단계가 **도구의 후보 종류와 상관없이** 들어간다 |
| `params` | 각 `default` | 선언된 것만. **선택된 어느 단계에도 적용되지 않는 파라미터를 명시하면 거절**한다(`param_not_applicable`) — 값이 무시된 채 결과에 적히는 것을 막는다 |
| `clean` | `false` | warm 트리 대신 일회 클론(§11.4) |
| `unwatched` | `"keep"` | 시작 전에 아무도 조회하지 않을 때. `"keep"` 은 유지, `"drop"` 은 30분 뒤 취소(§8.3) |

도구별로 고를 수 있는 종류:

| 도구 | `steps`·`tags` 로 고를 수 있는 종류 | 아무것도 고르지 않으면 |
|---|---|---|
| `build` | `build` · `package` | `default = true` 인 build |
| `test` | `test` · `scenario` | `default = true` 인 test |
| `run` | `build` · `package` · `test` · `scenario` | `default = true` 인 build + test |

어느 도구든 `after`·fixture 의존과 `artifacts` 의 생산 단계는 종류와 상관없이 따라 들어온다. 고를 수 없는 종류의 id 를 `steps` 에 주면 `build`·`test` 는 `wrong_kind` 로 거절한다(「run 으로 부르라」). **제외(§8.2)까지 적용한 뒤 setup 말고 돌 단계가 하나도 없으면 거절**한다(`nothing_to_run`).

`plan` 은 위에 `"tool": "build" | "test" | "run"` 을 더한다. `job_status`·`job_log`·`artifacts`·`cancel_job` 은 `{"job": "j000123"}`(작업 번호 또는 task_id), `job_log` 는 `step`·`tail`(기본 100)·`grep` 을 더한다.

### 7.4 결과

완료 결과(`structuredContent`, 각 도구는 `outputSchema` 를 선언한다):

```json
{
  "job_id": "j000123",
  "task_id": "t_5q2m8x0v3k7c9r1n4b6z8w2y0p",
  "tool": "run",
  "verdict": "pass",
  "failure_class": null,
  "summary": "example-app main(3f2a91c) run 통과 — build-apk, test-shared. 산출물 1개.",
  "project": "example-app",
  "ref": "main",
  "commit": "3f2a91c…",
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
- **Claude 가 알아야 할 것은 전부 `structuredContent` 에** — 요약도 `summary` 로. Agent SDK 문서는 `structuredContent` 가 있으면 `content` 텍스트를 Claude 에 넘기지 않는다고 한다(원격 서버도 그런지는 확인 안 함). 같은 요약을 `content` 텍스트에도 싣는다.
- 실패면 `summary` 첫 줄에 실패 분류·실패한 단계·**첫 에러 줄**, `excluded` 가 있으면 그 수를 적는다.
- 산출물은 `resource_link` 블록으로도 싣는다(Claude 에게는 이름·URI 한 줄 — 자동으로 받아지지 않는다).

### 7.5 요청(task)과 작업의 대응

**호출 하나 = task 하나, task 는 작업 하나를 따른다.** 중복 병합(§8.3)되면 **여러 task 가 한 작업을 따른다**(다대일). 식별자는 둘: `job_id`(`j000123`, 작업마다, 사람이 말하는 번호)와 `task_id`(128비트 난수, 호출마다 — Tasks 확장은 task ID 가 **추측 불가**일 것을 요구한다). 둘 다 **인증된 팀원이면 누구나** 조회·취소할 수 있다. 누가 열었는지는 감사 로그에 남기지만 접근을 막는 데 쓰지 않는다.

필드 규칙:

- `CreateTaskResult` 와 `tasks/get` 결과에 **`taskId`·`status`·`createdAt`·`lastUpdatedAt`·`ttlMs` 가 모두** 있어야 한다. 하나라도 없으면 Claude Code 는 `Unsupported result type 'task'` 로 실패한다.
- `tasks/get` 의 `resultType` 은 `"complete"`, 완료면 `result` 에 `CallToolResult` 전체.
- **`statusMessage` 는 매번 싣는다** — 진행을 알리는 유일한 수단이고, 빠지면 Claude Code 가 `/tasks` 표시를 지운다.
- **DB 에 커밋한 뒤에 task 를 돌려준다**(명세: `tasks/get` 이 답할 수 있기 전에 돌려주지 말 것). 작업 저장소는 `synchronous=FULL` 로 연다(§16.4).

| 작업 상태(§8.1) | task `status` | `statusMessage` 예 |
|---|---|---|
| intake | working | `접수: git fetch · 레시피 a1b2c3d` |
| provisioning | working | `툴체인: node 24.18.0 설치 중` |
| ready | working | `대기열 2번째 — 앞: j000121 example-app build` |
| running | working | `build-apk (1/2) · 3분 10초` |
| collecting | working | `산출물 수집 1/1` |
| done | completed | — |
| merged | (따르는 작업의 상태) | `j000118 과 같은 요청 — 그 작업을 따른다` |
| cancelled · queue_timeout | cancelled | — |
| interrupted | completed(`isError: true`) | 서비스 재시작으로 중단됨 |

시간·수명:

- **응답을 먼저.** Claude Code 의 요청별 타이머는 첫 응답 바이트까지 돌고 값은 60초·서버 도구 타임아웃·`MCP_TIMEOUT` 중 가장 큰 것. `tools/call` 은 입력 검증·1차 중복 병합·DB 커밋까지만 하고 task 를 **JSON 한 번**으로 돌려준다. 프로젝트 fetch 는 접수 단계에서 한다.
- `pollIntervalMs` 5000(Claude Code 가 250~30000 으로 자른다).
- **`ttlMs` = 작업 레코드 보관 기간(30일).** 산출물 보관(7일)보다 길다 — 산출물이 지워진 뒤에도 `tasks/get` 은 「완료, 산출물 만료」 를 답한다.
- 모르는·만료된 ID 는 `-32602`(명세). Claude Code 는 5분 재시도 후 「결과 모름」 으로 보고하므로, **보관 기간 안의 레코드는 재시작에도 잃으면 안 된다.**

취소:

- **`tasks/cancel`(task 하나)** → 그 task 를 작업에서 **뗀다**. 작업을 따르는 다른 task 가 남아 있으면 작업은 계속 돈다 — 같은 빌드를 기다리던 다른 사람의 결과를 빼앗지 않는다. 따르는 task 가 하나도 안 남고 작업이 아직 안 끝났으면 작업도 취소한다.
- **`cancel_job`(작업)** → 작업 자체를 취소한다. 따르던 task 전부가 `cancelled`, 결과에 누가 취소했는지 적는다.
- 작업 취소는 Job Object 종료, fixture `down`, 빌드 계정 잔류 프로세스 정리(§9.2)까지 한다. 이미 끝난 작업은 바꾸지 않는다.

task 를 못 쓰는 클라이언트(서브에이전트, VS Code 확장, `claude -p` — 요청에 tasks 확장 선언이 없음):

- 막고 기다리지 않는다. 작업을 만들고 일반 결과 `{"accepted": true, "done": false, "job_id": "…", "summary": "아직 끝나지 않았다. job_status 로 확인하라. 시작 전에 아무도 조회하지 않아도 유지된다(unwatched=keep)"}`.
- `-32021 MissingRequiredClientCapability` 로 거절하는 대안은 서브에이전트가 빌드를 아예 못 걸게 하므로 택하지 않았다.

### 7.6 결과 크기

Claude Code 는 텍스트 결과가 5만 자를 넘으면 파일로 빼고, `isError` 결과는 약 1만 1천 자를 넘으면 가운데를 자른다.

- 완료 결과는 **8천 자 이하.** 실패면 분류·실패 단계·첫 에러 줄과 앞뒤 몇 줄·로그 위치를 앞에.
- 긴 로그는 `job_log` 의 `tail`·`grep` 으로만.
- 산출물은 결과에 넣지 않는다 — HTTP(§12).

### 7.7 거절

task 를 만들기 전에 거절하는 경우. `isError: true` 일반 결과로 돌려준다(Claude 에게 메시지가 그대로 보인다).

| 코드 | 언제 | 메시지 예 |
|---|---|---|
| `unauthorized` | 토큰 없음·틀림 | (HTTP 401) |
| `unknown_project` | | `프로젝트 "eb" 가 없다. list_projects: …` |
| `ref_not_allowed` | 허용 목록 밖 | `"release/x" 는 허용 ref 가 아니다: main, feature/*, fix/*` |
| `recipe_invalid` | 레시피 오류(호출 검증, 또는 `recipe_edit` 저장 전 검사) | `recipe.toml:42: step "test-shared": unknown key "timeot_min"` |
| `unknown_recipe_ref` | 없는 시도 브랜치 | `recipe/x 가 없다 — recipe_history 로 브랜치를 확인하라` |
| `trial_ref_not_allowed` | `recipe/*` 밖 | |
| `recipe_conflict` | `recipe_edit` 의 `base` 이후 브랜치가 움직임 | `recipe/x 는 그 사이 a1b2c3d 로 바뀌었다 — 다시 읽고 고쳐라` |
| `forbidden` | 팀원이 `main` 편집·반영·되돌리기 | `main 반영은 관리자만 — recipe/* 브랜치에서 고치고 recipe_promote 를 요청하라` |
| `unknown_step` · `wrong_kind` · `unknown_tag` · `unknown_artifact` | | `"test-shared" 는 test 단계다 — test 나 run 으로 부르라` |
| `unknown_param` · `invalid_param` · `param_not_applicable` | | `backend_url 은 선택된 단계(package)에 적용되지 않는다 — build-apk 에만 쓰인다` |
| `needs_unavailable` | 능력이 없는 단계를 이름으로 고름 | `"ui-test" 는 display 가 필요하다 — 이 러너에는 없다` |
| `nothing_to_run` | 제외 뒤 돌 단계가 없음 | `고른 단계 1개가 모두 제외됐다(ui-test — display 필요). 돌릴 것이 없다` |
| `queue_full` | 그 사용자의 시작 전 작업이 한도 이상 | `시작 전 작업이 5개다(j000130 …) — 끝나거나 취소한 뒤 다시` |
| `disk_low` | 정리 후에도 여유 < 하한 | `디스크 여유 64 GB < 100 GB — 관리자 확인 필요` |
| `draining` | 업그레이드 대기 | `cimon 이 업그레이드를 기다리는 중 — 새 작업을 받지 않는다` |

---

## 8. 작업 모델

### 8.1 상태 — 접수와 실행을 나눈다

```
[접수 — 러너를 쓰지 않는다, 동시 2]               [실행 — 러너 1개]
intake --> provisioning --> ready --> running --> collecting --> done(verdict)

done 전의 어느 상태에서든 --> cancelled | merged | queue_timeout
running·collecting 에서     --> interrupted (서비스 재시작)
```

| 상태 | 하는 일 | 러너 |
|---|---|---|
| `intake` | 프로젝트·형제·(시도 실행이면) 레시피 mirror fetch → ref 를 커밋으로 고정·허용 브랜치 확인 → (프로젝트 레시피면 `git show` 로 읽어 검증) → `version_file` 읽기(같은 방식) → 선택·닫기·제외 → **계획 기록** → 2차 중복 병합 | 안 씀 |
| `provisioning` | managed 설치·`probe`, base 검사. 자동 설치가 허용되지 않은 도구가 없으면 여기서 멈추고 알린다(§10.4) | 안 씀 |
| `ready` | 러너 대기. 순서는 **요청 시각** | — |
| `running` | 워크스페이스 준비 → 단계(§9.1) | 씀 |
| `collecting` | provenance, 링크 | 씀 |
| `merged` | 2차 병합으로 다른 작업에 합쳐졌다. 이 작업의 task 들은 그 작업을 따른다 | — |
| `queue_timeout` | 시작 전 상태로 `max_queue_hours` 를 넘겼다 | — |

**나눈 이유**: 러너를 잡기 전에 끝낼 수 있는 일(fetch, 레시피 검증, 툴체인 설치)을 러너를 잡은 뒤에 하면 러너가 놀고, 커밋을 모르니 커밋 기준 중복 병합을 할 수 없다.

접수는 네트워크·디스크 일이라 빌드와 동시에 돌려도 된다(`intake_concurrency`, 기본 2). 같은 툴체인 버전을 두 접수가 동시에 설치하지 않도록 `(kind, version)` 잠금을 둔다. 정리기는 끝나지 않은 작업이 쓰는 툴체인 버전을 지우지 않는다. 실행 중인 작업이 쓰는 bare mirror 에 fetch 하는 것은 git 의 ref 잠금으로 안전하다.

### 8.2 판정과 실패 분류

| 판정 | 언제 |
|---|---|
| `pass` | 돈 단계가 전부 `pass` 이고, 요청한 산출물이 전부 수집됨. 돈 단계가 하나 이상(setup 제외) |
| `fail` | 코드·레시피 때문에 해야 할 일을 못 했다 |
| `inconclusive` | 판정에 필요한 것을 얻지 못했다 — 환경·인프라 때문에 코드를 시험하지 못했거나, 관측 창 안에 답을 듣지 못했다 |

`pass` 가 아니면 **`failure_class`** 로 누구의 일인지 가른다. 「다시 돌려 볼까」 와 「코드를 고쳐야 하나」 를 사람이 추측하지 않게 하기 위해서다.

| `failure_class` | 판정 | 예 | 누가 볼 일 |
|---|---|---|---|
| `code` | `fail` | 컴파일 에러, 테스트 실패, 0개 실행, 금지 출력, 산출물 누락, fixture 의 서비스가 기동 중 종료됨 | 브랜치 작성자 |
| `recipe` | `fail` | 레시피 오류, 판정·검사 스크립트 오류, git 추적 파일에 맞는 산출물 glob | 레시피 담당 |
| `environment` | `inconclusive` | base 툴체인 없음, 자동 설치가 허용되지 않은 도구 없음, 디스크 부족 | 관리자 |
| `infra` | `inconclusive` | fetch·다운로드 실패, fixture 등록 실패, 서비스 재시작 | 관리자 또는 재시도 |
| `unobserved` | `inconclusive` | 관측 창 안에 응답 없음, 관측 데이터 없음 | 시험 설계(창 길이 등) |

- 작업의 판정은 단계 판정의 최악이다(`fail` > `inconclusive` > `pass`). `failure_class` 는 처음 `pass` 가 아니게 된 단계의 것.
- 제외(`excluded`)·setup 생략(`skipped`)은 판정을 바꾸지 않는다. 결과에 적힌다. **돈 단계가 없으면 `pass` 가 아니다**(호출 때 `nothing_to_run` 으로 거절).
- `pass` 가 아니면 `isError: true`.

### 8.3 대기열

**순서**: 러너 동시 1. `ready` 작업을 요청 시각 순서로 꺼낸다. 우선순위는 1단계에 없다.

**중복 병합 — 두 번 한다.** 같은 빌드를 두 번 돌리지 않되, 병합 판정에 필요한 정보가 생기는 시점에 맞춘다.

| 시점 | 키 | 대상 | 하는 일 |
|---|---|---|---|
| 1차: 호출 때(응답 전) | `(project, ref 이름, 도구, 정렬한 steps·tags·artifacts, 기본값을 채운 params, clean, recipe_ref 또는 고정한 레시피 커밋)` | `intake`·`provisioning`·`ready` 인 작업 | 새 task 를 그 작업에 붙인다 |
| 2차: 접수 끝(커밋 확정 뒤) | 위에서 ref 이름 대신 **커밋**, 레시피 커밋 대신 **레시피 커밋·sha256** | `ready`·`running` 인 작업 | 이 작업을 `merged` 로 끝내고 task 들을 그 작업에 붙인다. 같은 커밋·같은 레시피·같은 선택이라 결과가 같다 |

- 끝난 작업과는 병합하지 않는다 — 다시 돌려 달라는 요청일 수 있다.
- 병합된 task 의 결과에는 `deduplicated: true` 와 따르는 작업 번호가 실린다.
- 취소는 task 단위로 뗀다(§7.5) — 병합이 다른 사람의 작업을 대신 취소하게 만들지 않는다.

**대기 정책 — 시작 전 작업을 아무도 보지 않을 때**

- **기본은 유지한다.** 긴 빌드 뒤에 줄 세워 두고 노트북을 닫아도, 다음 날 `job_status` 로 결과를 찾을 수 있어야 한다(원칙 5).
- 호출자가 `unwatched: "drop"` 을 주면, 시작 전에 30분 동안 아무도(`tasks/get`·`job_status`) 조회하지 않을 때 취소한다(이유 「아무도 기다리지 않음」). 병합된 작업은 붙은 task 가 **모두** `drop` 일 때만.
- 안전판: 시작 전 상태로 `max_queue_hours`(12시간)를 넘기면 `queue_timeout`. 사용자당 시작 전 작업이 `max_queued_per_user`(5개) 이상이면 새 호출을 `queue_full` 로 거절.
- 디스크 여유가 하한 아래면 정리기를 먼저 돌리고, 그래도 모자라면 거절(§16.4).

### 8.4 서비스 재시작

- 접수 단계(`intake`·`provisioning`)였던 작업은 접수를 **처음부터 다시** 한다. 접수의 일은 모두 다시 해도 같은 결과를 낸다.
- `ready` 작업은 그대로 기다린다.
- `running`·`collecting` 이던 작업은 이어서 돌리지 않는다 → `interrupted`(`inconclusive`/`infra`). 그 작업의 warm 슬롯 setup 상태를 지우고(§11.3), 빌드 계정 잔류 프로세스와 fixture 를 정리한다(§9.2).

---

## 9. 실행

### 9.1 작업 하나의 순서

1. **접수**(러너 밖, §8.1): fetch → 커밋 고정 → 레시피 → 계획 → 툴체인. 여기까지 끝난 작업만 `ready` 가 된다.
2. **시작 점검**(러너): 빌드 계정 잔류 프로세스가 없는지 확인하고, 있으면 정리하고 경고를 남긴다(§9.2).
3. **워크스페이스**: warm 슬롯 고르기·갱신 또는 clean 클론, 형제 트리(§11).
4. **running**: 단계마다
   1. 환경 구성(§9.4)
   2. 요청된 산출물과 판정 리포트의 **사전 정리**(§12.2, §9.6)
   3. 그 단계가 쓰는 fixture 가 아직 안 떴으면 `up` → `ready`
   4. 빌드 계정으로, Job Object 안에서 실행, 시간 제한
   5. 판정기 실행 → 단계 결과
   6. 산출물 수집(서비스 계정이 복사·sha256·검사)
   7. 그 fixture 를 쓰는 마지막 단계였으면 증거 보관(`collect`) → `down` → 사라졌는지 확인
5. **collecting**: `provenance.json`, 링크 발급.
6. **done**: 판정 기록. 남은 fixture 정리, Job Object 종료, **빌드 계정 잔류 프로세스 정리·확인**, `tmp\j<id>\` 삭제.

**한 작업 안의 단계는 차례로 돈다** — 서로 기대지 않는 단계도. 큰 C++ 빌드 하나가 CPU·RAM 을 다 쓰고, 로그가 섞이지 않아 실패 원인을 한 단계에서 읽을 수 있다. 가벼운 단계끼리 병렬은 §17.

### 9.2 프로세스와 계정

- 러너는 단계마다 **빌드 계정 `cimon-build` 의 토큰으로** 자식 프로세스를 띄운다(`LogonUser`(batch) + `CreateProcessAsUser`, 일시정지로 만든 뒤 Job Object 에 넣고 재개). 빌드 계정 비밀번호는 서비스 계정만 읽는 자격 증명 저장소에 둔다. 서비스 계정에 필요한 권한(「프로세스 수준 토큰 바꾸기」·「프로세스의 메모리 할당량 조정」)은 **확인 필요**(M2).
- Job Object(`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, breakaway 불허). 취소·시간 초과는 Job Object 통째로 종료.
- 단계가 끝난 뒤 Job Object 에 프로세스가 남아 있으면 종료하고 경고로 남긴다(빌드 데몬 등). 1단계는 속도보다 격리 — MSBuild 노드 재사용·Gradle 데몬을 끈다(레시피의 `env`).
- **Job Object 로 못 잡는 것**: 작업 스케줄러·서비스로 띄운 프로세스는 Job Object 밖에 있다. 프로젝트 도구가 스택을 「호출자보다 오래 살게」 띄우는 경우가 그렇다. 그래서 서비스는 Job Object 에만 기대지 않고:
  - **작업이 끝날 때마다 빌드 계정이 소유한 모든 프로세스를 찾아 종료하고, 남은 것이 없는지 확인한다.** 빌드 계정이 등록한 예약 작업도 지운다. 남았던 것은 결과에 경고로 적는다.
  - 다음 작업 시작 전에 다시 확인한다(§9.1 의 2).
  - 빌드 계정은 빌드만 하는 계정이라, 그 계정의 프로세스를 모두 죽여도 다른 것이 깨지지 않는다 — 계정을 나눈 이유 중 하나다.
- 예약 작업으로 뜬 프로세스는 호출자의 환경이 아니라 **계정에 저장된 환경**을 받는다. 그런 도구에 격리 홈을 주려면 빌드 계정의 사용자 환경 변수에 고정 경로를 두고, 서비스가 작업마다 그 내용을 비운다(러너가 하나씩만 돌리므로 성립). 어느 변수가 필요한지는 레시피 저장소의 프로젝트 노트가 정한다.
- 예약 작업을 S4U 로 등록하는 도구는 그 계정에 「일괄 작업으로 로그온」 권한이 있어야 돈다. 빌드 계정에 준다. `doctor` 가 검사한다.

### 9.3 명령 해석

- `run` 은 argv 배열이고 셸을 거치지 않는다.
- **첫 원소에 경로 구분자가 있을 때만**(예: `./gradlew.bat`) 작업 폴더 기준으로 찾는다. 이름만 있으면 구성한 `PATH` 에서만 찾는다. 저장소가 커밋한 `cargo.exe` 같은 파일이 고정한 도구를 가로채지 못하게 하기 위해서다.
- `.bat`·`.cmd` 는 Rust 표준 라이브러리의 배치 파일 실행(인자 이스케이프 포함)에 맡긴다. 직접 `cmd /c` 문자열을 만들지 않는다. `cmd.exe` 가 인자를 다시 해석한다는 점은 남는다 — 파라미터 값의 모양 검증(recipe-spec §8)이 그 부분을 막는다.

### 9.4 환경 변수

자식 환경은 **빈 상태에서 새로 만든다.** 순서대로 겹친다.

1. 시스템 기본값: `SystemRoot`·`windir`·`ComSpec`·`PATHEXT`·`OS`·`SystemDrive`·`ProgramFiles`·`ProgramFiles(x86)`·`ProgramW6432`·`ProgramData`·`CommonProgramFiles`·`CommonProgramFiles(x86)`·`CommonProgramW6432`·`NUMBER_OF_PROCESSORS`·`PROCESSOR_*`·`COMPUTERNAME`. 많은 도구가 이 값으로 Visual Studio·설치 프로그램을 찾는다(vswhere 경로 등).
2. 빌드 계정의 사용자 값: `USERPROFILE`·`HOMEDRIVE`·`HOMEPATH`·`LOCALAPPDATA`·`APPDATA`·`USERNAME`·`USERDOMAIN`
3. `TEMP`/`TMP` → `tmp\j<id>\`
4. `PATH` = 계획이 쓰는 툴체인의 `bin`(레시피 선언 순서) + `System32` 계열
5. 툴체인 `exports`
6. 캐시: `caches\<project>\` 아래로 `CARGO_HOME`·`npm_config_cache`·`UV_CACHE_DIR`·`GRADLE_USER_HOME`·`NUGET_PACKAGES`. `RUSTUP_HOME` 은 공유·읽기 전용
7. `[isolate]` 홈
8. 형제 환경 변수
9. 레시피 `[env]` → (clean 작업이면) `[workspace].clean_env` → fixture `env` → 단계 `env`
10. 파라미터 `env`(그 단계가 `applies_to` 에 있을 때)

**주지 않는 것**: `CARGO_TARGET_DIR`, `RUSTFLAGS` 는 레시피가 명시하지 않는 한 주지 않는다(일부 프로젝트 스크립트가 깨진다). 비밀은 어떤 경로로도 주지 않는다 — 빈 환경에서 시작하므로 레시피가 명시하지 않는 한 가지 않는다.

`USERPROFILE` 아래의 전역 설정을 읽는 도구는 6·7 에서 위치를 돌려 막는다. 돌리지 못한 것이 새는 지점이다 — 발견하면 레시피 `[isolate]` 에 추가한다.

### 9.5 출력

- stdout·stderr 는 단계 로그 파일로 그대로(서비스가 씀). 단계 로그는 **200 MB 상한** — 넘으면 앞 50 MB·뒤 50 MB 를 남기고 잘린 사실을 적는다. 메모리에는 끝 200줄.
- 「첫 에러 줄」 은 툴체인별 기본 패턴 + 레시피 `error_patterns` 로 찾는다.
- `fail_on_output` 에 걸린 줄이 있으면 종료 코드가 0 이어도 `fail`/`code`.

### 9.6 판정기 · 검사

| 판정기 | 구현 메모 |
|---|---|
| `exit` | 종료 코드 |
| `cargo-test` | 종료 코드 + `test result: ok. N passed; M failed; K ignored` 줄 합산. `passed < min_tests` 면 `fail` |
| `junit` | 종료 코드 + **단계 전에 `reports` glob 을 지운 뒤** 새로 생긴 XML 만 합산. 미리 지우는 이유: 증분 빌드 도구(Gradle 등)가 입력이 그대로면 테스트를 「이미 최신」 으로 건너뛰는데, 리포트가 남아 있으면 그 상태가 유지된다. 지우면 도구가 테스트를 다시 돌리거나 캐시에서 복원해 리포트를 새로 쓴다. 옛 리포트를 인정하면 조용한 통과가 된다 |
| `script` | 레시피 저장소의 판정 스크립트를 빌드 계정으로 실행. 입력: 단계 종료 코드·`${step.out}`·레시피가 준 인자. 출력: stdout JSON `{"verdict", "failure_class", "reason", "metrics"}`. **종료 코드가 0 이 아니거나 JSON 이 틀리면 `inconclusive`/`recipe`** — 판정기가 망가진 것을 통과로 넘기지 않는다 |

| 검사 | 구현 메모 |
|---|---|
| `apk-signer` | `apksigner verify --print-certs`(Android SDK build-tools) → 인증서 SHA-256·subject |
| `pe-version` | exe 의 VERSIONINFO |
| `script` | 레시피 저장소의 검사 스크립트. 출력 JSON 을 그대로 `inspect` 에 싣는다. 실패하면 그 산출물에 검사 실패를 적는다 |

판정·검사 스크립트가 서비스 바깥(레시피 저장소)에 있는 이유: 특정 프로젝트의 결과 형식을 아는 코드를 서비스에 넣지 않기 위해서다(원칙 1).

---

## 10. 툴체인

### 10.1 머신 준비 때 설치하는 것

`cimon setup`(관리자 1회)이 `toolchains\_bootstrap\` 에 **rustup·uv** 를 설치한다. 둘은 다른 툴체인을 설치하는 도구라 카탈로그 밖에 있다. 버전은 `cimon.toml [bootstrap]`.

### 10.2 두 층

| 층 | 무엇 | 누가 |
|---|---|---|
| **base** | 관리자 권한·대용량·재부팅·EULA 가 필요한 것(예: Visual Studio Build Tools, Git for Windows, Android SDK, 설치 파일 제작 도구) | 관리자, 미리 |
| **managed** | 사용자 공간에 버전별로 나란히 설치되는 것(예: Rust, Node, Python, JDK, 빌드 보조 CLI) | 서비스 계정, 접수 단계에서 |

종류 목록과 설치 방법은 레시피 저장소의 `toolchains.toml`(recipe-spec §16). **새 도구 종류는 기존 설치 방식으로 받을 수 있으면 카탈로그 추가만으로 된다.**

### 10.3 설치 규칙

- 설치는 **서비스 계정**이 하고, 빌드 계정은 `toolchains\` 를 읽기만 한다. 한 브랜치가 다른 작업이 쓸 툴체인을 바꿔치지 못한다.
- **체크섬 출처가 없는 managed 도구는 설치하지 않는다.**
- 설치 뒤 `probe` 로 기대 버전인지 확인한다. `expect` 는 정확한 버전 문자열이어야 한다(「openjdk version」 처럼 아무 버전이나 맞는 패턴 금지). 다르면 설치 실패.
- rustup 의 자동 설치에 기대지 않는다 — 접수가 미리 깔고, 없으면 접수에서 실패한다.

### 10.4 자동 설치 허용 — 승인 질문은 없다

- `cimon.toml [toolchains].auto_approve` 에 든 종류는 접수 단계에서 설치한다.
- 나머지는 **관리자가 `cimon provision <project>`**(또는 `--kind <k> --version <v>`)로 미리 설치한다. 설치돼 있지 않으면 접수에서 작업을 멈추고 `inconclusive`/`environment` 로 끝낸다: 「jdk 17.0.19+10 이 없다 — 관리자가 `cimon provision example-app` 을 실행하거나 auto_approve 에 추가」.
- MCP 로 승인 질문(elicitation)을 보내지 않는 이유: Claude Code 는 대화형 터미널에서만 그 질문을 띄우고, 서브에이전트·비대화형 호출자는 답할 길이 없다. 승인 대기가 작업을 붙잡는 상태도 생긴다. 설치는 드물게 일어나므로 관리자 명령으로 충분하다.
- 카탈로그에 **새로 추가된 종류**는 `auto_approve` 에 없으므로 처음엔 관리자 설치를 거친다.

### 10.5 정리

30일 동안 어떤 작업도 쓰지 않은 managed 버전은 정리기가 지운다(`toolchain_installs.last_used_at`). 끝나지 않은 작업이 쓰는 버전은 지우지 않는다.

### 10.6 기록

작업마다 실제로 쓴 툴체인을 `probe` 출력으로 결과·`provenance.json` 에 남긴다.

### 10.7 카탈로그 밖에서 받는 것

프로젝트의 빌드 도구가 **스스로** 받는 것(빌드 시스템 wrapper 배포판, 번들러가 받는 설치 파일 제작 도구, 프로젝트 스크립트가 받는 보조 바이너리, 패키지 의존성)은 cimon 의 체크섬 보장 밖이다.

- 레시피 저장소의 프로젝트 노트에 알려진 것을 적는다.
- 서비스는 작업마다 캐시·홈 디렉터리에 **새로 생긴 실행 파일**을 기록해 provenance 에 남긴다(무엇이 들어왔는지라도 알 수 있게).
- 체크섬을 강제하려면 프로젝트 쪽에서 고친다(예: Gradle wrapper 의 `distributionSha256Sum`).

---

## 11. 워크스페이스

### 11.1 mirror 와 클론

- **bare mirror**: 프로젝트마다 `mirrors\<id>.git`. 접수에서 `git fetch --prune`(서비스 계정, GitHub 토큰).
- **ref 해석**: fetch 뒤 커밋으로 고정. 작업 도중 브랜치가 움직여도 그 커밋을 짓는다.
- **작업 트리는 독립 클론**이다 — mirror 를 alternates 로 참조해(`git clone --reference`) 객체는 mirror 의 것을 읽기만 하고, 인덱스·작업 트리는 클론 자신의 것이다. `git worktree` 를 쓰지 않는 이유: worktree 는 메타데이터를 mirror 안에 쓰므로 빌드 계정이 mirror 에 쓰기 권한을 가져야 한다.
- 클론의 원격은 로컬 mirror 다. 빌드 계정은 GitHub 자격 증명 없이 작업 트리를 다룰 수 있다.

### 11.2 warm 슬롯

- 프로젝트마다 warm 클론을 `warm_slots` 개 둔다(기본 1 — `cimon.toml [workspace].warm_slots_per_project`, `project.toml` 의 `warm_slots` 가 덮어쓴다).
- 고르는 규칙: 마지막으로 같은 **ref 이름**을 지은 슬롯 → 없으면 가장 오래 안 쓴 슬롯.
- 갱신: `git checkout --force <sha>` → `git clean -ffdx` 에 `[workspace].keep` 을 제외로. 증분 빌드가 된다.
- 슬롯이 1개면 같은 프로젝트의 다른 브랜치를 번갈아 지을 때마다 다시 빌드를 많이 한다. 1단계는 받아들인다 — 자주 번갈아 짓는 프로젝트만 올린다.

### 11.3 setup 상태와 무효화

- warm 슬롯별로 setup 단계의 마지막 성공(커밋·`inputs` 해시·툴체인 해시·단계 정의 해시)을 `warm_state` 에 기록해 `when = "cold" | "changed"` 를 판정한다.
- **그 슬롯에서 setup 이 실패하거나, 작업이 취소·시간 초과·중단되면 그 슬롯의 `warm_state` 를 지운다.** 다음 작업은 cold 로 시작한다. setup 이 반쯤 다시 만든 트리 위에서 「변경 없음」 으로 setup 을 건너뛰면, 엉뚱한 실패가 나거나 그 커밋이 선언하지 않은 의존성으로 통과할 수 있다.
- clean 작업은 `warm_state` 를 읽지도 쓰지도 않는다.

### 11.4 clean 작업

- 일회 클론 `ws\<id>\j<id>\` 에서 처음부터, 빌드 디렉터리도 새로. 끝나면 지운다.
- **clean 이 지우지 않는 것**을 명시한다: `caches\<project>\`(패키지 캐시), project 범위 홈, 빌드 도구의 원격·로컬 빌드 캐시. 결과의 `workspace` 에 「clean — 캐시 유지」 로 적는다.
- 빌드 캐시가 산출물·테스트 결과까지 복원하는 도구는 레시피의 `[workspace].clean_env` 로 clean 작업에서만 끈다(예: Gradle `-Dorg.gradle.caching=false`).

### 11.5 형제 트리

- 레시피 저장소의 `project.toml` 이 `[[sibling]]` 으로 선언한 저장소를 같은 커밋 규칙으로 클론해 넘긴다. 쓴 커밋을 결과에 적는다.
- 놓는 자리: 기본은 `ws\_sibling\<project>\j<id>\`. `place = "beside"` 면 **작업 트리의 부모 디렉터리** 아래 `<sibling id>`(`ws\<project>\<sibling id>\`) — 프로젝트 스크립트가 형제를 `../<이름>` 같은 상대 경로로만 찾을 때 쓴다. warm 슬롯끼리 같은 자리를 공유하므로 작업마다 다시 맞춘다(러너가 하나씩만 돌린다).
- 넘기는 변수는 여럿일 수 있다(`env = ["A_ROOT", "A_DIR"]`) — 스크립트마다 다른 이름으로 찾는 경우.

### 11.6 자격 증명

- 서비스 계정: 등록된 저장소를 읽는 GitHub fine-grained 토큰(자격 증명 관리자 + credential helper). mirror fetch 에 쓴다. **SSH 키는 두지 않는다** — git 설정 `url."https://github.com/".insteadOf "ssh://git@github.com/"`.
- 빌드 계정: 기본은 **GitHub 자격 증명이 없다.** 빌드 중에 비공개 저장소를 git 으로 받는 프로젝트(git 의존성)가 있으면, 그 저장소만 읽는 **별도 토큰**을 빌드 계정에 둔다. 어느 저장소인지는 레시피 저장소의 프로젝트 노트에 적는다.
- `core.longpaths=true`.

---

## 12. 산출물

### 12.1 왜 MCP 로 넘기지 않나

HTTP MCP 응답은 16 MiB 를 넘으면 실패한다(Claude Code 문서). 1단계 대상 프로젝트의 산출물은 27 MB~270 MB 로 전부 그보다 크다. 결과에는 목록과 주소만, 파일은 HTTP 로.

### 12.2 수집 — 사전 정리

- **요청된 산출물의 glob 에 맞는 기존 파일을, 생산 단계가 돌기 전에 지운다.** 단계가 끝난 뒤 맞는 파일은 그 단계가 만든 것이다. 같은 폴더에 쌓인 옛 버전이나 이전 작업의 exe 를 새 것으로 넘기는 것을 막는다.
- 「단계 시작 뒤 수정된 파일만 인정」 은 쓰지 않는다 — 증분 빌드는 바뀌지 않은 파일을 다시 쓰지 않으므로 멀쩡한 빌드를 실패로 만든다.
- 비용(다시 링크·다시 번들)은 **요청된 산출물에만** 든다.
- git 추적 파일은 지우지 않는다 — 맞으면 그 단계 `fail`/`recipe`.
- 개수가 `expect`·`expect_min` 과 다르면 그 단계 `fail`/`code`.
- 수집은 **서비스 계정**이 한다: 작업 트리에서 `artifacts\j<id>\` 로 복사, sha256 계산, `inspect` 실행. 빌드 계정은 `artifacts\` 에 쓸 수 없다 — 다른 작업의 산출물이나 해시를 바꿔치지 못한다.

### 12.3 다운로드

- `GET https://cimon.<tailnet>.ts.net/artifacts/j<id>/<name>?t=<token>`. 링크는 `cimon.toml [server].public_url` 로 만든다.
- 토큰 = HMAC(`config\link.key`, 작업·이름·만료). 기본 1시간, **`artifacts` 도구가 부를 때마다 새로** 발급. 링크 토큰이 있으면 `Authorization` 헤더 없이 받는다. 링크는 tailnet 밖에서는 열리지 않는다.
- `Range` 지원.
- 보관 기간이 지나면 `410 Gone` + 「j000123 의 산출물은 2026-11-02 에 만료돼 지웠다」.
- 보관 기본 7일(`project.toml` 의 `retention_days` 로 프로젝트별).

### 12.4 provenance

`artifacts\j<id>\provenance.json`: 프로젝트, ref, 커밋, 형제 커밋, 레시피 출처·커밋·sha256, 파라미터, 툴체인 버전, 워크스페이스(모드·슬롯), 단계별 결과·시간, 산출물 sha256·검사 결과, 카탈로그 밖에서 새로 생긴 실행 파일(§10.7), 러너 이름, `trial`, `signed: false`.

### 12.5 서명

산출물은 **서명 안 된 테스트 빌드**다. 서명 키는 이 머신에 없다(§15). 플랫폼별 서명 관련 사실(예: Android debug 서명은 빌드 계정의 키로 된다)은 `inspect` 로 결과에 드러낸다.

---

## 13. 데이터 모델

SQLite(WAL, `synchronous=FULL`). 시각은 UTC ISO 8601.

```sql
CREATE TABLE jobs (
  id               INTEGER PRIMARY KEY,          -- j000123 = printf('j%06d', id)
  tool             TEXT NOT NULL,                -- build | test | run | scaffold
  project          TEXT NOT NULL,
  ref              TEXT,
  commit_sha       TEXT,                         -- 접수에서 확정
  recipe_source    TEXT,                         -- central | project
  recipe_commit    TEXT,                         -- 중앙 출처면 호출 때 검증에 쓴 커밋을 고정
  recipe_sha256    TEXT,
  trial            INTEGER NOT NULL DEFAULT 0,
  request_json     TEXT NOT NULL,                -- 검증된 입력
  plan_json        TEXT,                         -- 선택·닫기·제외 결과
  ref_key          TEXT NOT NULL,                -- 1차 병합 키(§8.3)
  commit_key       TEXT,                         -- 2차 병합 키 — 접수 끝에 채운다
  merged_into      INTEGER REFERENCES jobs(id),
  state            TEXT NOT NULL,                -- intake | provisioning | ready | running | collecting
                                                 -- | done | merged | cancelled | queue_timeout | interrupted
  verdict          TEXT,                         -- pass | fail | inconclusive
  failure_class    TEXT,                         -- code | recipe | environment | infra | unobserved
  status_message   TEXT,
  summary          TEXT,
  result_json      TEXT,                         -- 완료 structuredContent
  created_by       TEXT NOT NULL,
  cancelled_by     TEXT,
  workspace_mode   TEXT,                         -- warm | clean
  workspace_slot   INTEGER,
  fixture_state    TEXT,                         -- 떠 있는 fixture(재시작 정리용)
  created_at       TEXT NOT NULL,                -- ready 순서의 기준
  ready_at         TEXT,
  started_at       TEXT,
  finished_at      TEXT,
  last_updated_at  TEXT NOT NULL,
  expires_at       TEXT NOT NULL                 -- 레코드 보관(ttlMs 와 같다)
);
CREATE INDEX jobs_ready ON jobs(state, created_at);
CREATE INDEX jobs_ref_key ON jobs(ref_key) WHERE state IN ('intake','provisioning','ready');
CREATE INDEX jobs_commit_key ON jobs(commit_key) WHERE state IN ('ready','running');

CREATE TABLE tasks (                             -- 호출 하나 = task 하나(§7.5)
  task_id         TEXT PRIMARY KEY,              -- 128비트 난수, base32
  job_id          INTEGER NOT NULL REFERENCES jobs(id),
  requested_by    TEXT NOT NULL,
  client_info     TEXT,
  via_task        INTEGER NOT NULL,              -- tasks 확장 선언 여부(아니면 accepted 경로)
  unwatched       TEXT NOT NULL DEFAULT 'keep',  -- keep | drop
  created_at      TEXT NOT NULL,
  last_polled_at  TEXT,
  detached_at     TEXT,
  detached_reason TEXT
);
CREATE INDEX tasks_job ON tasks(job_id);

CREATE TABLE steps (
  job_id        INTEGER NOT NULL REFERENCES jobs(id),
  seq           INTEGER NOT NULL,
  step_id       TEXT NOT NULL,
  kind          TEXT NOT NULL,
  result        TEXT NOT NULL,                   -- pending | running | pass | fail | inconclusive | not_run | skipped
  failure_class TEXT,
  reason        TEXT,
  exit_code     INTEGER,
  started_at    TEXT,
  finished_at   TEXT,
  excerpt       TEXT,                            -- 첫 에러 줄 주변
  judge_json    TEXT,
  PRIMARY KEY (job_id, seq)
);

CREATE TABLE excluded (
  job_id  INTEGER NOT NULL REFERENCES jobs(id),
  step_id TEXT NOT NULL,
  reason  TEXT NOT NULL,
  PRIMARY KEY (job_id, step_id)
);

CREATE TABLE artifacts (
  job_id       INTEGER NOT NULL REFERENCES jobs(id),
  name         TEXT NOT NULL,
  path         TEXT NOT NULL,
  bytes        INTEGER NOT NULL,
  sha256       TEXT NOT NULL,
  inspect_json TEXT,
  created_at   TEXT NOT NULL,
  expires_at   TEXT NOT NULL,
  deleted_at   TEXT,
  PRIMARY KEY (job_id, name)
);

CREATE TABLE toolchain_installs (
  kind TEXT NOT NULL, version TEXT NOT NULL, root TEXT NOT NULL, checksum TEXT,
  installed_at TEXT NOT NULL, last_used_at TEXT NOT NULL,
  PRIMARY KEY (kind, version)
);

CREATE TABLE job_toolchains (
  job_id INTEGER NOT NULL REFERENCES jobs(id), kind TEXT NOT NULL, version TEXT NOT NULL, probe TEXT,
  PRIMARY KEY (job_id, kind)
);

CREATE TABLE warm_slots (
  project TEXT NOT NULL, slot INTEGER NOT NULL,
  last_ref TEXT, last_commit TEXT, last_used_at TEXT,
  PRIMARY KEY (project, slot)
);

CREATE TABLE warm_state (
  project TEXT NOT NULL, slot INTEGER NOT NULL, step_id TEXT NOT NULL,
  last_ok_commit TEXT, inputs_sha256 TEXT, toolchains_hash TEXT, def_sha256 TEXT,
  at TEXT NOT NULL,
  PRIMARY KEY (project, slot, step_id)
);

CREATE TABLE users (
  name TEXT PRIMARY KEY, token_sha256 TEXT NOT NULL UNIQUE,
  role TEXT NOT NULL DEFAULT 'member',           -- member | admin — 레시피 main 반영·되돌리기는 admin(§6.4)
  created_at TEXT NOT NULL, disabled_at TEXT
);

CREATE TABLE audit (
  id INTEGER PRIMARY KEY, at TEXT NOT NULL, user TEXT NOT NULL,
  action TEXT NOT NULL,                          -- call:<tool> | cancel | download | provision
                                                 -- | recipe_edit | recipe_promote | recipe_revert | recipe_import
  job_id INTEGER, recipe_commit TEXT, tailnet_login TEXT, detail TEXT
);
```

---

## 14. 설정 — `cimon.toml`

비밀을 담지 않는다(사용자 토큰은 `users` 테이블에 해시로, 링크 키는 `config\link.key`).

```toml
[server]
bind = "127.0.0.1:8770"                            # tailscale serve 만 앞에서 받는다(§15.3)
public_url = "https://cimon.<tailnet>.ts.net"      # 산출물 링크의 기준 주소
allowed_hosts = ["cimon.<tailnet>.ts.net"]         # rmcp 기본값은 loopback 뿐. serve 가 넘기는 Host 값 — M0 에서 확인

[tailscale]
serve_target = "http://127.0.0.1:8770"             # doctor 가 `tailscale serve status --json` 과 대조
require_funnel_off = true                          # Funnel 이 켜져 있으면 서비스가 시작을 거부한다
audit_identity_headers = true                      # Tailscale-User-Login 을 감사 로그에(인증에는 쓰지 않는다)

[paths]
root = 'D:\cimon'

[accounts]
service = "cimon"
build = "cimon-build"

[recipes]
store = 'D:\cimon\recipes.git'                     # 빌드 머신 안 레시피 저장소(bare git). 외부 원격 없음
default_branch = "main"
trial_prefix = "recipe/"
trial_branch_idle_days = 30                        # 반영 안 된 시도 브랜치 정리
allowed_repo_prefixes = ["https://github.com/<org>/"]   # 시도 브랜치의 새 프로젝트가 가리킬 수 있는 저장소

[runner]
name = "cimon-01"
platform = "windows-x86_64"
capabilities = []                                  # display·device·gpu 없음
concurrency = 1

[base]                                             # base 툴체인 위치(카탈로그 check.path_from)
git = 'C:\Program Files\Git'
android_sdk = 'C:\Android\Sdk'
inno_setup = 'C:\Program Files (x86)\Inno Setup 6'

[bootstrap]
rustup = "<고정 필요>"
uv = "<고정 필요>"

[toolchains]
auto_approve = ["rust", "node", "python", "cmake", "task"]   # 나머지는 관리자 cimon provision
unused_days = 30

[queue]
intake_concurrency = 2
max_queued_per_user = 5
max_queue_hours = 12
drop_unwatched_minutes = 30

[workspace]
warm_slots_per_project = 1

[retention]
artifacts_days = 7
records_days = 30
link_minutes = 60

[disk]
min_free_gb = 100                                  # 이 아래면 정리기 → 그래도 모자라면 새 작업 거절
cache_max_gb_per_project = 50                      # 넘으면 오래 안 쓴 항목부터 지운다
log_max_mb_per_step = 200
```

---

## 15. 보안

### 15.1 두 계정

| 계정 | 하는 일 | 쓸 수 있는 곳 |
|---|---|---|
| `cimon`(서비스) | MCP·HTTP 서버, 작업 저장소, mirror fetch, 툴체인 설치, 산출물 수집·서빙 | `D:\cimon` 전부 |
| `cimon-build`(빌드) | 단계 실행만 | `ws\`·`caches\`·`homes\`·`tmp\`·`logs\j*\*\out\`. `mirrors\`·`toolchains\`·`artifacts\` 는 읽기만, `state\`·`config\` 는 접근 불가 |

- 둘 다 비관리자. 관리자 설치는 사람만.
- 이렇게 나누면 브랜치의 빌드 스크립트가 **작업 저장소·다른 작업의 산출물과 해시·링크 서명 키·툴체인·서비스 실행 파일을 바꿀 수 없다.** 같은 계정이었다면, 동료가 받을 산출물을 바꿔치고 DB 의 sha256 까지 고쳐 해시 대조를 통과시킬 수 있었다.

### 15.2 위협과 대응

| 위협 | 대응 |
|---|---|
| 아무나 빌드를 건다 | 사용자별 Bearer 토큰(`cimon token create <이름>`), DB 에는 해시. 모든 호출을 감사 로그에 |
| 허용하지 않은 저장소·브랜치를 짓는다 | `project.toml` 의 `allowed_refs`. 밖이면 즉시 거절 |
| 브랜치·시도 브랜치가 권한을 넓힌다 | 등록된 프로젝트의 `project.toml` 은 언제나 레시피 저장소 `main` 에서 읽는다. 시도 브랜치의 새 프로젝트는 `allowed_repo_prefixes` 안의 저장소만 |
| 레시피를 고칠 수 있는 사람 | 레시피 = 빌드 머신에서 실행되는 명령이다. 시도 브랜치(`recipe/*`)는 팀원 누구나 고치고 시도 실행할 수 있다 — 토큰을 가진 팀원 전체를 그만큼 믿는다는 전제다. `main` 반영·되돌리기는 관리자(`users.role = admin`)만, diff 를 보고. 모든 편집은 작성자와 함께 커밋·감사 로그에 남는다 |
| 레시피 저장소가 사라진다 | 외부 원격이 없으므로 매일 `git bundle` 백업(§16.4). 레시피 저장소는 빌드 계정이 읽지 못한다 — 체크아웃(`recipes\<commit>\`)만 읽기 전용으로 |
| 빌드 스크립트가 cimon 을 해친다 | 빌드 계정 분리와 디렉터리 권한(§15.1) |
| 빌드 스크립트가 다른 프로젝트를 오염시킨다 | 캐시를 프로젝트별로(`caches\<project>\`). 내려받을 때만 체크섬을 보는 패키지 캐시를 공유하면, 한 브랜치가 풀어 둔 소스를 다른 프로젝트 빌드가 그대로 쓰게 된다 |
| 빌드 스크립트가 비밀을 훔친다 | 서명 키·릴리스 키·배포 자격 증명을 두지 않는다. 빌드 계정에는 기본적으로 GitHub 자격 증명도 없다(§11.6) |
| 빌드 스크립트가 네트워크로 퍼진다 | tailnet 에서 `tag:cimon` 이 출발하는 grant 를 두지 않고, 기본 규칙(모두→모두)을 지운다(§15.3). 빌드 계정의 아웃바운드는 Windows 방화벽으로 GitHub·패키지 레지스트리·사내 아티팩트 저장소만 허용한다 — 사무실 LAN 의 장비로 나가지 못하게 |
| 파라미터로 명령을 주입한다 | 선언된 파라미터만, 모양 검증(`pattern`·범위·열거). argv 로 넘긴다(§9.3) |
| 저장소가 도구를 가로챈다 | 경로 구분자가 없는 명령은 구성한 `PATH` 에서만 찾는다(§9.3) |
| 산출물 링크가 샌다 | 링크 토큰 1시간. 링크 자체가 tailnet 주소라 tailnet 밖에서는 열리지 않는다 |
| 서버가 인터넷에 노출된다 | 접속 경로는 Tailscale 하나. 서버는 `127.0.0.1`, 서버용 인바운드 규칙 없음, **Funnel 금지** |
| 팀원 PC 에서 토큰이 샌다 | `headersHelper` 가 DPAPI 로 암호화한 파일에서 읽는다(§15.4). 토큰은 사용자별이라 하나만 끌 수 있다 |
| 빌드 머신의 다른 프로세스가 `127.0.0.1:8770` 을 부른다 | 토큰이 없으면 401. Tailscale 신원 헤더는 **인증에 쓰지 않는다** — 로컬 프로세스가 헤더를 꾸밀 수 있고, `serve` 가 들어오는 헤더를 지우는지 문서에 없다 |

**두 겹**: 네트워크(Tailscale 기기 인증 + ACL)와 서버(사용자 토큰). **받아들이는 위험**: 빌드 계정은 모든 프로젝트의 작업 트리를 읽을 수 있다 — 등록된 팀 내부 저장소만 짓는다는 전제로 받아들인다. 줄이려면 프로젝트별 빌드 계정이나 VM 으로 나눈다(§17).

### 15.3 Tailscale

```
팀원 PC --(tailnet, WireGuard)--> cimon.<tailnet>.ts.net:443
                                   tailscale serve (HTTPS, 인증서 자동)
                                     --> http://127.0.0.1:8770  cimon
```

- **빌드 머신**
  - **unattended 모드**(`tailscale up --unattended=true`, 또는 트레이 Preferences → Run unattended). 없으면 로그아웃·재부팅 때 끊긴다. 켠 Windows 계정이 그 기기의 Tailscale 설정을 소유하므로 **관리자 계정으로** 켠다.
  - 태그 `tag:cimon`. 관리 콘솔에서 **키 만료가 꺼져 있는지** 확인한다(기본 180일마다 재인증).
  - `tailscale serve --bg --https=443 http://127.0.0.1:8770`. `--bg` 는 재부팅·`tailscale down/up` 뒤에도 유지된다. `serve` 는 `http://127.0.0.1` 대상만 프록시한다.
  - tailnet 에 **HTTPS 인증서를 켜 둔다**(`serve` 의 전제).
  - **Funnel 은 켜지 않는다.** 서비스는 시작할 때와 `doctor` 에서 `tailscale serve status --json` 을 읽어 Funnel 이 켜져 있으면 **시작을 거부한다**.
- **ACL**(초안 — tailnet 관리자가 실제 그룹·계정으로 바꾼다):

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
      // tag:cimon 을 src 로 하는 grant 는 두지 않는다 — 빌드 머신은 tailnet 의 다른 기기에 닿지 못한다
    ]
  }
  ```

- **신원 헤더**: `serve` 는 `Tailscale-User-Login`·`Tailscale-User-Name` 을 붙인다(태그 기기·Funnel 에서 온 요청에는 붙지 않는다). 서버는 **감사 로그에만** 남기고, 토큰 사용자와 다르면 경고를 남긴다.
- **성능**: 직접 연결이 안 되면 DERP 중계로 우회해 큰 산출물 다운로드가 느려진다. `tailscale status` 로 확인한다. 끊기면 `Range` 로 이어받는다.
- **Tailscale 이 내려가면** 서비스에 닿을 수 없다. 받아들인다 — 관리자는 빌드 머신 콘솔로 들어간다.

### 15.4 팀원 PC 의 토큰 — `headersHelper`

`claude mcp add --header` 는 토큰을 Claude Code 설정 파일에 평문으로 남긴다. 대신 `headersHelper`(연결할 때 실행해 헤더 JSON 을 받는 명령, 제한 10초)로 DPAPI 암호화 파일에서 읽는다.

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
3. Claude Code 등록(경로는 절대 경로로):
   ```
   claude mcp add-json cimon "{\"type\":\"http\",\"url\":\"https://cimon.<tailnet>.ts.net/mcp\",\"headersHelper\":\"powershell -NoProfile -File C:\\\\Users\\\\<me>\\\\.cimon\\\\header.ps1\"}"
   ```

Claude Code 는 인증 오류가 나면 `headersHelper` 를 다시 실행하므로 바뀐 토큰을 바로 집는다.

---

## 16. 운영

### 16.1 설치

- `cimon setup`(관리자 1회): 디렉터리와 권한(§5), 서비스 계정 `cimon`·빌드 계정 `cimon-build`(둘 다 비관리자, 서비스 계정은 「서비스로 로그온」, 빌드 계정은 「일괄 작업으로 로그온」), Windows 서비스 등록(자동 시작·실패 시 재시작), rustup·uv bootstrap, git 전역 설정, 빌드 계정 아웃바운드 방화벽 규칙.
- Tailscale(관리자 1회, §15.3): 설치 → 관리자 계정으로 로그인·unattended → 태그·키 만료 확인 → `serve --bg` → `tailscale serve status` 로 확인.
- 서비스는 세션 0 에서 돈다.

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
| Tailscale | unattended, `tag:cimon`, 키 만료 꺼짐, `serve --bg` 443 → `127.0.0.1:8770`, Funnel 꺼짐, HTTPS 인증서 켜짐 |
| 방화벽 | 서버용 인바운드 규칙 없음. 관리자 원격 데스크톱은 tailnet 경유(ACL `tcp:3389`)로만 — RDP 허용 범위를 `100.64.0.0/10` 으로. 빌드 계정 아웃바운드 제한(§15.2) |
| base 툴체인 | 레시피 저장소의 카탈로그가 요구하는 것 — `cimon doctor` 가 빠진 목록을 낸다 |
| git(서비스 계정) | `core.longpaths=true`, `url."https://github.com/".insteadOf "ssh://git@github.com/"`, credential helper + 읽기 토큰 |

### 16.3 팀원 PC 준비

1. Tailscale 설치 → 회사 계정으로 로그인. tailnet 관리자가 `group:cimon-users` 에 넣는다.
2. Claude Code v2.1.304 이상인지 확인(MCP tasks).
3. 관리자가 `cimon token create <이름>` 으로 토큰을 발급해 전달한다(메신저 평문 금지).
4. §15.4 대로 토큰을 저장하고 `headersHelper` 로 등록한다.
5. 확인: `/mcp` 에 cimon 이 연결됨, `list_projects` 가 프로젝트 목록을 돌려준다.

퇴사·분실 때는 tailnet 에서 기기를 빼고 토큰을 끈다(`users.disabled_at`).

### 16.4 관측 · 정리 · 백업

- `GET /healthz` — 큐 길이, 실행 중 작업, 디스크 여유, 레시피 `main` 커밋, 마지막 백업 시각. tailnet 주소로 열어 Tailscale 경로까지 한 번에 본다.
- `cimon doctor` — base 툴체인, 두 계정과 권한, 디스크, GitHub 자격 증명(만료 14일 전 경고), 레시피 저장소(`git fsck` + `main` 검사), 마지막 백업이 하루를 넘겼는지, Tailscale(연결, `serve` 대상, Funnel 꺼짐).
- 정리기(1시간마다, 그리고 디스크 하한에 닿을 때):
  1. 만료 산출물·로그·홈 삭제(작업 레코드는 `records_days` 까지)
  2. 안 쓰는 툴체인 삭제(§10.5)
  3. 프로젝트별 캐시가 `cache_max_gb_per_project` 를 넘으면 오래 안 쓴 항목부터
  4. 그래도 하한 아래면 가장 오래 안 쓴 warm 슬롯을 지운다(다음 작업은 cold)
  5. 그래도 하한 아래면 새 작업 거절(`disk_low`)
- 백업(하루 한 번, 각 14개 보관):
  - 작업 저장소: `VACUUM INTO 'backup\cimon-<날짜>.db'`(파일 복사가 아니라 — WAL 모드에서 복사하면 깨질 수 있다).
  - **레시피 저장소: `git bundle create backup\recipes-<날짜>.bundle --all`.** 외부 원격이 없으므로 이것이 유일한 사본이다. 백업 폴더는 빌드 머신 밖(사내 파일 서버 등)으로도 복사한다 — 어디로 보낼지는 확인 필요(§19.1).
  - 복구: `cimon recipe restore <bundle>` — 번들에서 `recipes.git` 을 다시 만든다.
  - 백업이 실패하면 `/healthz`·`doctor` 에 드러난다.

### 16.5 허브 업그레이드

드레인: 새 작업을 거절하고(`draining`), 실행 중인 작업이 끝나면 멈춘다. 레시피 변경에는 필요 없다 — 레시피는 레시피 저장소에서 작업마다 읽는다.

---

## 17. 확장 — 1단계 뒤

| 확장 | 지금 준비된 것 | 할 일 |
|---|---|---|
| 새 프로젝트 | 레시피·프리셋·카탈로그, 시도 실행, `recipe_edit` | 레시피 저장소에 추가 |
| 레시피 리뷰 화면 | `recipe_diff`·`recipe_history` | tailnet 안 읽기 전용 웹 화면에 diff·반영 버튼 |
| 레시피 저장소의 외부 사본 | 평범한 git 저장소 | 원하면 사내 비공개 원격으로 단방향 push(백업 겸용). 1단계는 번들 백업만 |
| 레시피 초안 자동 생성 | `scaffold_recipe` 도구 정의 | 구현(저장소 코드를 실행하지 않고 표지 파일만 읽는다, `allowed_repo_prefixes` 안만) |
| 프로젝트 레시피(`recipe_source = "project"`) | 형식·검증 시점 정의 | 구현 |
| 화면이 있는 러너 | 단계의 `needs = ["display"]`, 러너 `capabilities` | 대화형 세션에서 도는 러너 에이전트 — 레시피 변경 없이 그 단계가 돈다 |
| 러너 여러 대 · Linux · macOS | `platforms`, 러너 `platform`·`capabilities` | 원격 러너 에이전트(같은 작업 모델) — tailnet 에 `tag:cimon-runner` 로 붙고 ACL 로 허브와만 통신 |
| 동시 실행(작업 여러 개) | 접수/실행 분리, 작업별 홈·클론, warm 슬롯 | 러너별 `concurrency`, 슬롯 단위 잠금, 빌드 계정을 작업 수만큼 |
| 단계 병렬(한 작업 안) | 단계 사이 의존이 `after` 로 명시돼 있다 | 단계에 자원 표시(예: `weight = "light"`)를 두고 가벼운 독립 단계끼리 동시에 |
| 자동 트리거 | 같은 작업 모델 | 기본 브랜치 폴링(webhook 은 tailnet 안이라 못 받는다) + 커밋 상태 표시 + 실패 알림 |
| 프로젝트별 격리 강화 | 빌드 계정 분리 | 프로젝트별 빌드 계정 또는 VM |
| 새 판정·설치 방식 | 판정기·installer 를 이름으로 고르는 구조, `script` 판정기 | 코드 추가 |

---

## 18. 구현 순서

각 단계의 「끝났다」 기준에 **실패 경로를 하나 이상** 넣었다. 성공 경로만 보면 조용한 성공을 놓친다.

| 단계 | 내용 | 끝났다고 보는 기준 |
|---|---|---|
| M0 | MCP 서버 뼈대(rmcp, `server/discover`) + 인증 + `list_projects` + Tailscale `serve` | 사무실 밖 팀원 PC 에서 tailnet 주소로 도구 목록이 보인다. tailnet 밖 기기에서는 접속이 안 된다. 토큰 없이 부르면 401. Funnel 을 켜면 서비스가 시작을 거부한다 |
| M1 | 레시피 저장소(`recipes.git`, `recipe import/export/show`) + `cimon-recipe` + `recipe check` + `describe_project`·`plan` | 사내 레시피 폴더를 `import` 하면 `main` 커밋이 생기고 `describe_project` 가 그 커밋으로 답한다. `examples/recipes/` 와 레시피가 검사를 통과한다. 오타 난 키·모르는 `after`·범위 버전·`nothing_to_run`·`param_not_applicable` 이 파일·줄과 함께 거절되고, **검사에 실패한 폴더는 `import` 되지 않는다** |
| M2 | 작업 저장소 + 접수/대기/실행 + 두 계정 + Job Object + task 대응(`build`·`test`·`run`) | task 로 돌고 `/tasks` 에 진행이 보인다. 5분을 넘는 작업이 끝까지 간다. 같은 요청 두 번 → 작업 하나에 task 둘, 하나를 `tasks/cancel` 해도 다른 쪽은 결과를 받는다. **빌드 단계가 `state\cimon.db`·`artifacts\` 에 쓰려 하면 실패한다.** 빌드 계정으로 띄운 예약 작업 프로세스가 작업 뒤에 남지 않는다 |
| M3 | 툴체인 관리자 + `cimon provision` | 빈 머신에서 레시피 저장소 전 프로젝트 provision 이 끝난다. 체크섬이 틀린 파일은 설치되지 않는다. auto_approve 밖 도구가 없으면 `inconclusive`/`environment` 와 명령이 나온다 |
| M4 | 워크스페이스(mirror·독립 클론·warm 슬롯·clean·형제) + 레시피 저장소 프로젝트 build·test | 모든 프로젝트 기본 build·test 가 `pass`. 일부러 깨뜨린 브랜치는 `fail`/`code` 와 첫 에러 줄. setup 을 중간에 취소한 뒤 다음 작업이 cold 로 시작한다. 레시피 저장소 노트의 음성 대조 시험(형제 저장소를 없애면 실패 등)이 실패한다 |
| M5 | 레시피 편집 도구(`recipe_edit`·`diff`·`history`·`promote`·`revert`) + 역할(member·admin) | Claude 가 `recipe/*` 브랜치에 단계를 추가하고 시도 실행한 뒤 관리자가 반영하면 다음 작업이 새 레시피로 돈다. **깨진 레시피는 저장되지 않는다**, 낡은 `base` 로 고치면 `recipe_conflict`, 팀원이 `main` 을 고치면 `forbidden` |
| M6 | 산출물(사전 정리·수집·검사·HTTP) | 받은 파일 sha256 이 결과와 같다. 옛 버전이 쌓인 폴더에서도 새 것 하나만 잡힌다 |
| M7 | fixture + `script` 판정기 + 시나리오 | 시나리오가 판정을 낸다. 관측 창을 줄이면 `inconclusive`/`unobserved`. fixture 의 서비스가 기동 중 죽으면 `fail`/`code` 와 서비스 이름. 취소해도 스택이 남지 않는다 |
| M8 | 운영 — 정리기·드레인·doctor·감사·백업(작업 저장소 + 레시피 번들)·복구 | 7일 지난 산출물이 `410`. 레시피 저장소를 지우고 어제 번들로 `restore` 하면 그 시점 `main` 으로 돌아온다. 디스크를 채우면 정리기가 먼저 돈다 |

---

## 19. 미결정 · 결정

### 19.1 확인 필요(서비스 쪽)

| 항목 | 언제 | 메모 |
|---|---|---|
| 빌드 계정으로 자식 프로세스를 띄울 때 서비스 계정에 필요한 권한 | M2 | `CreateProcessAsUser` 의 권한 요구 |
| `serve` 가 넘기는 Host 헤더 값 | M0 | `allowed_hosts` |
| 빌드 계정 아웃바운드 방화벽 허용 목록 | M2 | 패키지 레지스트리 도메인 — 레시피 저장소 노트에서 모은다 |
| 레시피 저장소 git 구현 | M1 | `git2` 크레이트(libgit2) 또는 git CLI 호출 |
| 백업 번들을 보낼 빌드 머신 밖 위치 | 팀 | 레시피 저장소의 유일한 외부 사본이 된다 |
| tailnet 관리자 · 요금제 인원 · tailnet 이름, 기본 규칙 유무 | 팀 | §15.3 |
| 사무실 방화벽의 UDP(직접 연결) | 팀 | §15.3 |
| 빌드 머신 확보 · Visual Studio 라이선스 | 팀 | §16.2 |

프로젝트별 확인 항목은 레시피 저장소의 프로젝트 노트에 있다.

### 19.2 결정 기록

| 날짜 | 결정 | 이유 |
|---|---|---|
| 2026-10-25 | Windows 1대, 실행 엔진은 직접 구현(외부 CI 엔진 안 씀) | 러너 1대·하나씩이라 범위가 감당할 만하고, 에이전트 인터페이스와 「실패가 보이게」 하는 판정은 어차피 직접 만들어야 한다 |
| 2026-10-25 | 빌드·테스트 방법은 레시피(데이터)로, 레시피는 git 에서 직접 읽는다 | 새 프로젝트·테스트 수정에 코드 변경·재배포가 없게 |
| 2026-10-25 | 접속 경로는 Tailscale 하나 — 서버 `127.0.0.1` + `tailscale serve` HTTPS, Funnel 금지 | 외부에서도 쓰되 포트를 인터넷에 열지 않는다 |
| 2026-10-26 | 이름 cimon | zenmon 과 형제 이름. 「Simon says」 가 쓰임새와 맞는다 |
| 2026-10-26 | `run` 도구, `build`·`test` 는 그 줄임 | 「빌드해서 받고 테스트도」 를 호출 한 번으로 |
| 2026-10-26 | 접수(러너 밖)와 실행(러너)을 나눈다 | 러너가 놀지 않고, 커밋 기준 병합을 러너 전에 하기 위해 |
| 2026-10-26 | task 와 작업은 다대일, 취소는 task 단위로 뗀다 | 병합이 다른 사람의 작업을 대신 취소하게 만들지 않기 위해 |
| 2026-10-26 | 시작 전 작업은 기본 유지, 안전판은 12시간·사용자당 5개 | 결과는 서버가 갖는다(원칙 5) |
| 2026-10-26 | 한 작업 안의 단계는 차례로, warm 슬롯은 프로젝트당 기본 1 | 1단계 단순함 |
| 2026-10-26 | **서비스 저장소와 레시피 저장소를 나눈다** | 원격 빌드 서비스는 프로젝트를 몰라야 한다. 프로젝트 정보(고객사 포함)는 레시피 저장소에만 |
| 2026-10-26 | **레시피 저장소는 빌드 머신 안의 git, 편집은 MCP 도구** — GitHub 등 외부 저장소에 올리지 않는다 | 저장소를 하나 더 관리하지 않는다. git 이라 버전 고정·시도 브랜치·이력은 그대로. 대가: PR 리뷰 화면이 없고(`recipe_diff` 로 대신), 유일한 사본이라 번들 백업이 필수 |
| 2026-10-26 | 레시피 `main` 반영·되돌리기는 관리자만, 시도 브랜치는 팀원 누구나 | 레시피 = 빌드 머신 명령. 이전의 「저장소 push 권한 + main 보호」 와 같은 신뢰 수준 |
| 2026-10-26 | 프로젝트별 결과 판정은 `script` 판정기로 레시피 저장소에 | 특정 프로젝트의 결과 형식을 아는 코드를 서비스에 넣지 않는다 |
| 2026-10-26 | 빌드 계정 분리 | 빌드 스크립트가 서비스 상태·다른 작업의 산출물을 바꾸지 못하게 |
| 2026-10-26 | 실패 분류(`failure_class`) | 「다시 돌릴까」 와 「코드를 고칠까」 를 사람이 추측하지 않게 |
| 2026-10-26 | 승인 질문(elicitation) 대신 관리자 `cimon provision` | 비대화형 호출자가 답할 수 없고, 설치는 드물다 |
| 2026-10-26 | `scaffold_recipe`·프로젝트 레시피는 형식만 정의하고 구현은 2단계 | 1단계에 쓰는 곳이 없다 |

---

## 부록 A. 근거(외부, 2026-10-25~26 확인)

- Claude Code MCP 문서 — tasks, 요청별 타이머, 출력 한도, 인증, `headersHelper`: https://code.claude.com/docs/en/mcp
- Claude Agent SDK custom tools — `resource_link`, `structuredContent`: https://code.claude.com/docs/en/agent-sdk/custom-tools
- MCP 2026-07-28 changelog: https://modelcontextprotocol.io/specification/2026-07-28/changelog
- MCP Tasks 확장: https://modelcontextprotocol.io/extensions/tasks/overview, https://github.com/modelcontextprotocol/ext-tasks
- rmcp: https://github.com/modelcontextprotocol/rust-sdk(태그 `rmcp-v3.7.0`, `examples/servers/src/common/task_demo.rs`)
- Tailscale Serve: https://tailscale.com/kb/1312/serve, CLI: https://tailscale.com/kb/1242/tailscale-serve, unattended: https://tailscale.com/kb/1088/run-unattended
