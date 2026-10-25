# 레시피 명세 (schema 1)

- 상태: 초안 · 2026-10-26
- 이 문서는 **레시피 파일의 형식**을 정의한다. 서비스 구조는 [design.md](design.md). 예시는 가상의 프로젝트 `example-app`(Rust 백엔드 + Gradle Android 앱)과 `example-sim`(시뮬레이터 위 시나리오 시험)이다. 형식 예시 파일은 [`examples/recipes/`](../examples/recipes/).

---

## 1. 무엇이 레시피인가

cimon 이 프로젝트를 빌드·테스트하는 방법은 **전부 데이터(TOML)로** 적는다. 새 프로젝트를 추가하거나 테스트를 고칠 때 서비스 코드를 고치거나 다시 배포하지 않는다.

레시피는 cimon 이 빌드 머신 안에 갖는 **레시피 저장소**(git, `cimon.toml [recipes].store`)에 둔다. 외부 원격은 없고, 사람은 cimon 의 MCP 도구(`recipe_show`·`recipe_edit` …)나 관리자 명령(`cimon recipe import/export`)으로 읽고 고친다(design §6.4). 저장소 안의 배치:

| 파일 | 내용 |
|---|---|
| `toolchains.toml` | 툴체인 카탈로그 — 어떤 도구를 어디서 받아 어떻게 검증하나. **설치 항목은 `main` 의 것만 쓰인다**(§16.5) |
| `presets/<name>.toml` | 스택별 공통 조각(예: `rust-cargo`) |
| `projects/<id>/project.toml` | 프로젝트 등록 — 저장소, 허용 ref, 형제 저장소. **권한에 닿는 것 — `main` 의 것만 쓰인다** |
| `projects/<id>/recipe.toml` | 툴체인 요구, 단계, 산출물, 호출자 파라미터, fixture |
| `projects/<id>/*.toml` | `recipe.toml` 이 `include` 하는 조각(예: 시나리오 목록) |
| `scripts/`, `projects/<id>/scripts/` | 판정·검사 스크립트, 레시피가 쓰는 설정 파일 — `${recipes}` 로 참조 |

경로는 Windows 체크아웃에서도 같은 파일이어야 한다: 끝이 점이나 공백인 이름, 장치 이름(`NUL`·`CON`·`COM1`…, 확장자가 붙어도), `< > " | ? *`, 대소문자만 다른 두 경로는 저장되지 않는다 — 나중 파일이 리뷰한 파일을 덮어쓰게 되므로. 시도 브랜치 이름은 소문자만 쓴다.

### 1.1 버전

모든 파일 첫 줄에 `schema = 1`. 서비스는 아는 schema 만 읽고, 모르는 schema 는 그 프로젝트의 작업을 거절한다.

### 1.2 엄격함

- **모르는 키는 오류다.** 오타 난 키가 조용히 무시되는 것을 막는다.
- **툴체인 버전은 정확히 고정한다.** `"22"`·`"stable"`·`"^1.9"` 같은 범위는 쓰지 않는다. 범위는 base 층의 `min` 에만 허용한다(§5).
- 검증 오류는 **파일·줄·키**를 짚어 보고한다.

---

## 2. 레시피를 어디서 읽나

### 2.1 레시피 저장소

서비스는 빌드 머신 안의 레시피 저장소(bare git)를 **git 에서 직접** 읽는다. 서비스가 설치된 폴더의 파일은 읽지 않는다.

- 기본 브랜치는 `main`(`cimon.toml [recipes].default_branch`). `main` 은 아무도 직접 고치지 않는다 — `recipe/*` 브랜치에서 고치고 관리자가 `recipe_promote` 로 반영하면 **다음 작업부터** 쓰인다(design §6.5). 서비스 재시작은 필요 없다.
- **저장되는 커밋은 정적 검사(§15.1)를 통과한다.** 이번 변경이 새로 만든 오류가 있으면 저장하지 않는다(design §6.3).
- 작업은 레시피 커밋을 고정하고 결과에 적는다.
- **시도 실행**: 도구 호출에 `recipe_ref` 로 `recipe/<설명>` 브랜치를 주면 그 브랜치의 `recipe.toml`·include·프리셋·스크립트로 돈다. 결과에 `trial: true`.
  - **이미 등록된 프로젝트의 `project.toml` 은 `main` 의 것을 쓴다.** 시도 브랜치가 저장소 주소·허용 ref·형제 저장소를 바꾸지 못한다.
  - 시도 브랜치에만 있는 새 프로젝트는 `repo` 가 `cimon.toml [recipes].allowed_repo_prefixes` 에 맞을 때만 받는다.
  - **툴체인은 `main` 카탈로그 항목으로 설치된 것만 쓴다**(§16.5).

### 2.2 `recipe_source`

`project.toml` 의 `recipe_source` 가 `recipe.toml` 의 **단 하나의 출처**를 정한다. 두 곳을 다 보고 우선순위를 매기지 않는다.

| 값 | `recipe.toml` 을 읽는 곳 | 상태 |
|---|---|---|
| `"central"`(기본) | 레시피 저장소 `projects/<id>/recipe.toml`(`main` 또는 시도 브랜치) | 1단계 |
| `"project"` | 빌드하는 ref 의 `.cimon/recipe.toml` | **2단계** — 형식만 정의 |

- `"project"` 여도 **`project.toml` 은 레시피 저장소에만** 있다.
- 프리셋·툴체인 카탈로그·스크립트는 언제나 레시피 저장소에서 읽는다.

---

## 3. `project.toml`

```toml
schema = 1
id = "example-app"                        # 디렉터리 이름과 같아야 한다
display_name = "Example App"
repo = "https://github.com/<org>/example-app.git"
allowed_refs = ["main", "feature/*", "fix/*"]
default_ref = "main"
recipe_source = "central"
platforms = ["windows-x86_64"]
max_job_minutes = 90
warm_slots = 1

[[sibling]]                               # 이 프로젝트의 빌드가 읽는 다른 저장소
project = "example-core"                  # 등록된 프로젝트 id
ref = "main"
env = ["EXAMPLE_CORE_ROOT", "EXAMPLE_CORE_DIR"]
place = "beside"
```

| 키 | 타입 | 필수 | 뜻 |
|---|---|---|---|
| `id` | 문자열 | ● | `[A-Za-z0-9_.-]+`. MCP 도구의 `project` 인자 |
| `display_name` | 문자열 | | |
| `repo` | 문자열 | ● | HTTPS git URL. 서비스의 GitHub 토큰이 읽을 수 있어야 한다 |
| `allowed_refs` | 문자열 배열 | ● | 브랜치 이름 glob. `*` 는 `/` 를 넘지 않고 `**` 는 넘는다. 커밋 SHA 는 이 브랜치들 중 하나에서 닿을 때만 받는다 |
| `default_ref` | 문자열 | ● | `ref` 를 안 주면 이것 |
| `recipe_source` | `"central"`·`"project"` | | 기본 `"central"` |
| `platforms` | 문자열 배열 | | 기본 `["windows-x86_64"]`. 러너의 플랫폼과 맞아야 작업이 잡힌다 |
| `max_job_minutes` | 정수 | | 기본 120. 실행 단계(`running`)에 든 시간. 넘으면 남은 단계를 멈추고 `fail`/`code`(시간 초과) |
| `retention_days` | 정수 | | 산출물 보관 기간. 기본 7(design §16.4) |
| `warm_slots` | 정수 | | 증분 빌드용 warm 트리 개수. 기본은 `cimon.toml [workspace].warm_slots_per_project`(1) |
| `[[sibling]]` | 표 배열 | | 형제 저장소(§3.1) |

### 3.1 `[[sibling]]`

| 키 | 뜻 |
|---|---|
| `project` | 등록된 프로젝트 id. 그 `project.toml` 의 `repo` 를 쓴다 |
| `ref` | 형제의 ref. 작업 접수 때 커밋으로 고정하고 결과·중복 병합 키에 적는다 |
| `env` | 형제 트리 경로를 넘길 환경 변수 — 문자열 또는 배열. 스크립트마다 다른 이름으로 찾으면 모두 적는다 |
| `place` | `"isolated"`(기본, `ws\_sibling\<project>\j<id>\`) · `"beside"`(작업 트리의 **부모 디렉터리** 아래 `<sibling id>`). 프로젝트 스크립트가 형제를 `../<이름>` 같은 상대 경로로만 찾을 때 `beside` |

---

## 4. `recipe.toml` — 최상위

```toml
schema = 1
description = "…"
extends = ["rust-cargo"]          # presets/<name>.toml, 이 순서로 겹친다(§10)
include = ["scenarios.toml"]      # 같은 디렉터리의 조각(§10)

[vars]           # §4.1
[toolchains]     # §5
[workspace]      # §6
[env]            # §7.1
[isolate]        # §7.2
[account_env]    # §7.3
[params.<name>]  # §8
[[step]]         # §9
[fixture.<name>] # §11
```

### 4.1 `[vars]`

레시피 안에서 `${vars.<이름>}` 으로 쓰는 문자열 상수. 프리셋이 기본값을 두고 프로젝트가 덮어쓰는 용도다(예: `rust_dir`).

---

## 5. `[toolchains]`

키는 카탈로그(`toolchains.toml`)의 **종류 이름**, 값은 요구다. 카탈로그에 없는 종류는 오류다.

```toml
[toolchains]
rust    = { version_file = "backend/rust-toolchain.toml" }
jdk     = { version = "17.0.19+10" }
node    = { version = "24.18.0" }
python  = { version = "3.13.14", packages = ["pyyaml==6.0.3"], optional = true }
msvc    = { min = "17.11", components = ["Microsoft.VisualStudio.Component.VC.Tools.x86.x64"] }
```

| 필드 | 뜻 |
|---|---|
| `version` | 정확한 버전. managed 종류에 필수(`version_file` 이 없을 때) |
| `version_file` | 저장소 안의 파일에서 버전을 읽는다(접수 때 `git show` 로 — 접수 검사, §15.2). 지금은 `rust-toolchain.toml` 의 `toolchain.channel` 만 지원하고, 그 값도 정확한 버전이어야 한다. 파일이 없거나, **저장소 안의 다른 `rust-toolchain.toml` 이 다른 버전을 적고 있으면** 작업은 `fail`/`recipe` 다(한 작업이 둘 중 하나로 조용히 지어지지 않게) |
| `min` | base 종류의 최소 버전 |
| `packages` | `python` 전용 — 정확한 버전의 패키지 목록(`name==x.y.z`, extras 허용 — 옵션·URL·경로는 오류). `(python 버전, 패키지 목록)` 마다 uv 가상 환경을 하나 만든다 |
| `components` 등 | 종류별 추가 필드 — 카탈로그 정의를 따른다(§16) |
| `optional` | `true` 면 기본으로는 갖추지 않는다. 그것을 `uses` 로 적은 단계가 선택됐을 때만 갖추고 검사한다 |

선택된 단계들이 쓰는 툴체인만 갖춘다. 관리자 명령 `cimon provision <project>` 은 `optional` 을 포함해 전부를 갖춘다.

---

## 6. `[workspace]`

```toml
[workspace]
keep = ["backend/target", "app/.gradle", "app/**/build"]
clean_env = { GRADLE_OPTS = "-Dorg.gradle.daemon=false -Dorg.gradle.caching=false" }
```

| 키 | 뜻 |
|---|---|
| `keep` | warm 트리를 초기화(`git clean -ffdx`)할 때 **지우지 않는** 경로 glob. 증분 빌드용 |
| `clean_env` | **clean 작업에서만** 넣는 환경 변수. 빌드 캐시가 산출물·테스트 결과까지 복원하는 도구를 clean 에서 끄는 데 쓴다. `[env]` 의 같은 키를 **통째로** 바꾸므로 필요한 값을 다 적는다(§7.4) |

`keep` 에 든 경로라도 산출물·판정 리포트의 사전 정리(§9.6)는 적용된다.

---

## 7. 환경

### 7.1 `[env]`

```toml
[env]
MSBUILDDISABLENODEREUSE = "1"
SOME_PATH = "${ws}/tools"
```

모든 단계에 들어가는 환경 변수. 변수 확장(§13)을 한다. 서비스는 `CARGO_TARGET_DIR`·`RUSTFLAGS` 를 주지 않는다 — 레시피가 명시하면 주고, 검증 경고를 낸다.

### 7.2 `[isolate]`

```toml
[isolate]
APP_HOME = "job"
PKG_CACHE_HOME = "project"
```

「전역 설정을 읽는 도구」 의 홈을 서비스가 정한 곳으로 돌린다. 값이 범위다.

- `"job"`: `homes\j<id>\<NAME>\` — 작업마다 새로. 설정만 담는 홈에 쓴다.
- `"project"`: `homes\p\<project>\<NAME>\` — 그 프로젝트의 작업끼리 공유. 캐시가 큰 도구에 쓴다. clean 작업도 공유한다(design §11.4).

### 7.3 `[account_env]` — 예약 작업으로 뜨는 프로세스용

```toml
[account_env]
APP_HOME = "job"
```

예약 작업(작업 스케줄러)으로 프로세스를 띄우는 도구는 호출자의 환경을 물려주지 않는다 — 그 프로세스는 **빌드 계정에 저장된 사용자 환경 변수**를 받는다. `[account_env]` 에 적은 변수는 작업 시작 전에 cimon 이 빌드 계정의 사용자 환경 변수로 쓰고 작업 끝에 지운다(design §9.2).

- 값은 `[isolate]` 와 같은 범위(`"job"`·`"project"`)이고 **같은 이름이 `[isolate]` 에도 있어야 한다** — 직접 실행한 단계와 예약 작업이 같은 홈을 보게. 없으면 오류.
- 러너가 하나라 작업끼리 섞이지 않는다(동시 실행 확장 때 다시 본다 — design §17).

### 7.4 층과 교체 경고

단계 환경은 design §9.4 의 순서로 겹친다: `[env]` → (clean 작업이면) `[workspace].clean_env` → fixture `env` → 단계 `env` → 파라미터 `env`. **값을 합치지 않고 위 층이 키를 통째로 바꾼다.**

위 층의 값이 **아래 층의 값을 포함하지 않은 채** 같은 키를 바꾸면 정적 검사가 경고를 낸다 — 예: `[env] GRADLE_OPTS = "-Dorg.gradle.daemon=false"` 를 `clean_env` 의 `GRADLE_OPTS = "-Dorg.gradle.caching=false"` 가 바꿔 clean 작업에서만 데몬이 켜지는 경우. 위 층에 아래 층 값까지 다 적으면 경고가 사라진다.

---

## 8. `[params.<name>]` — 호출자가 고를 수 있는 값

도구 호출 때 `params` 로 넘길 수 있는 값. **선언하지 않은 파라미터는 거절한다.** 레시피가 허용한 모양의 값만 들어온다.

```toml
[params.backend_url]
type = "string"
pattern = '^https?://[A-Za-z0-9.-]+(:[0-9]+)?$'
default = "http://10.0.2.2:8800"
env = "APP_BACKEND_URL"
applies_to = ["build-apk"]
description = "debug APK 에 컴파일되는 백엔드 주소"

[params.jobs]
type = "int"
min = 1
max = 16
default = 8
applies_to = ["build-native"]          # 그 단계의 run 이 ${param.jobs} 로 쓴다
description = "빌드 병렬도"

[params.version]
type = "string"
pattern = '^[0-9]+\.[0-9]+\.[0-9]+$'
args = ["-Version", "${value}"]
applies_to = ["package"]
```

| 필드 | 뜻 |
|---|---|
| `type` | `string` · `int` · `bool` · `enum` |
| `default` | 없으면 「값 없음」. 값 없음이면 `env`·`args` 를 넣지 않는다 |
| `pattern` | `string` 의 정규식(전체 일치) |
| `min`·`max` | `int` 범위 |
| `values` | `enum` 의 허용 값 |
| `env` | 이 환경 변수로 `applies_to` 단계에 넘긴다 |
| `args` | `applies_to` 단계의 `run` **끝에** 덧붙인다. `${value}` 가 값 |
| `applies_to` | **필수.** 이 값을 실제로 쓰는 단계 id |
| `description` | `describe_project` 에 보인다 |

- **모든 파라미터는 소비처가 있어야 한다**: `env`·`args` 가 있거나, `applies_to` 의 단계가 `${param.<name>}` 을 쓴다. 셋 다 없으면 오류(값이 어디에도 안 들어간다).
- `${param.<name>}` 은 `applies_to` 에 든 단계에서만 쓸 수 있다 — 다른 단계에서 쓰면 오류. 값 없음인 파라미터를 `${param.x}` 로 참조하면 오류다(`default` 를 두라).
- **호출자가 명시한 파라미터가 선택된 어느 단계에도 적용되지 않으면 거절한다**(`param_not_applicable`). 값이 무시된 채 결과에 「적용함」 으로 적히는 것을 막는다.
- **단계가 그 값을 실제로 쓰는지는 레시피가 책임진다.** 프로젝트 스크립트가 같은 값을 내부에서 하드코딩해 덮어쓰면 그 단계는 `applies_to` 에 넣지 않는다. 의심되면 산출물 `inspect` 로 결과에 실제 값을 드러낸다.
- **비밀은 파라미터로 받지 않는다.** 결과와 감사 로그에 값이 남는다.
- 파라미터 값(기본값을 채운 것)은 결과에 기록되고, 중복 병합 키에 들어간다.

---

## 9. `[[step]]`

### 9.1 예

```toml
[[step]]
id = "build-apk"
kind = "build"
description = "Android debug APK"
run = ["./gradlew.bat", ":app:assembleDebug", "--console=plain", "--no-daemon"]
cwd = "${vars.gradle_dir}"
timeout_min = 20
tags = ["android"]

  [[step.artifact]]
  name = "app-debug.apk"
  glob = "app/app/build/outputs/apk/debug/app-debug.apk"
  expect = 1
  inspect = ["apk-signer"]
```

### 9.2 키

| 키 | 타입 | 기본 | 뜻 |
|---|---|---|---|
| `id` | 문자열 | 필수 | 레시피 안에서 고유. `[a-z0-9][a-z0-9-]*` |
| `kind` | `setup`·`build`·`test`·`package`·`scenario` | 필수 | §9.3 |
| `description` | 문자열 | | `describe_project`·결과에 보인다 |
| `run` | 문자열 배열 | 필수 | argv. 셸을 거치지 않는다. **첫 원소에 경로 구분자가 있을 때만**(`./gradlew.bat`) `cwd` 기준으로 찾고, 이름만 있으면 구성한 `PATH` 에서만 찾는다. `.bat`·`.cmd` 는 Rust 표준 라이브러리의 배치 실행에 맡긴다 |
| `shell` | `"bash"`·`"powershell"` | | `bash` 면 `bash <run…>`(Git Bash, base 층), `powershell` 이면 `powershell -NoProfile -ExecutionPolicy Bypass -File <run…>` |
| `cwd` | 문자열 | `${ws}` | 작업 트리 기준 상대 경로(`app`, `./app`, `${ws}/app`). `..` 로 작업 트리 밖에 나가면 오류 |
| `env` | 표 | | 이 단계에만 추가 |
| `timeout_min` | 정수 | 30 | 넘으면 Job Object 종료 → 단계 `fail`/`code`(시간 초과) |
| `after` | 문자열 배열 | `[]` | 먼저 끝나야 하는 단계 id(종류 무관) |
| `tags` | 문자열 배열 | `[]` | 호출자가 태그로 고를 때 |
| `default` | 불리언 | build·test 는 `true`, package·scenario 는 `false` | 호출자가 아무것도 고르지 않았을 때 들어가나 |
| `needs` | 문자열 배열 | `[]` | 러너 능력. `display`·`device`·`gpu`. 러너에 없으면 **제외하고 결과에 적는다**(§14) |
| `uses` | 문자열 배열 | `[]` | 이 단계가 쓰는 `optional` 툴체인 |
| `fixture` | 문자열 | | 이 단계 동안 떠 있어야 하는 fixture(§11) |
| `fail_on_output` | 문자열 배열 | `[]` | 출력에 이 문자열이 든 줄이 있으면 종료 코드가 0 이어도 `fail`/`code`. 「건너뜀」·「못 찾음」 같은 조용한 성공 문구를 잡는 데 쓴다 |
| `error_patterns` | 문자열 배열 | | 실패 요약에서 「첫 에러 줄」 을 찾을 정규식. 툴체인별 기본값에 더한다 |
| `judge` | 표 | `{ type = "exit" }` | 통과 판정 방식(§12) |
| `uncounted` | 불리언 | `false` | test·scenario 단계가 `exit` 판정기를 쓴다는 것 — 「몇 개 돌았는지 세지 않는다」 — 를 알고 쓴다는 표시. 없으면 경고(§12). 결과의 그 단계에 `uncounted: true` 가 실린다 |
| `when` | `"always"`·`"cold"`·`"changed"` | `"always"` | **setup 만.** §9.4 |
| `inputs` | 문자열 배열 | | **setup 만**, `when = "changed"` 일 때 지켜보는 파일 glob |
| `disabled` | 불리언 | `false` | `true` 면 없는 것으로 친다. 프리셋 단계를 끌 때 — 이때 다른 키는 생략할 수 있다. 앞 층에 없는 id 를 끄면 오류(오타로 프리셋 단계가 그대로 도는 것을 막는다). include 에서는 쓸 수 없다 |
| `[[step.artifact]]` | 표 배열 | | 이 단계가 만드는 산출물(§9.5) |

### 9.3 종류

| kind | 어느 도구가 고르나 | 비고 |
|---|---|---|
| `setup` | 자동 — 모든 build·test·run 작업의 맨 앞 | `when` 에 따라 돌거나 건너뛴다. 건너뛰면 결과에 「setup 생략: 변경 없음」 |
| `build` | `build` · `run` | |
| `package` | `build` · `run`(명시적으로 고르거나 그 산출물을 요청할 때) | 기본 미포함 |
| `test` | `test` · `run` | |
| `scenario` | `test` · `run`(명시적으로 고르거나 태그로) | fixture 가 필요한 긴 시험. 기본 미포함 |

### 9.4 setup 의 `when`

| 값 | 도는 때 |
|---|---|
| `always` | 매번 |
| `cold` | warm 트리를 새로 만들었거나, `clean` 작업이거나, 툴체인 버전이 지난번 성공 때와 다르거나, 이 단계 정의가 바뀌었거나, **지난 작업이 그 슬롯에서 실패·취소·중단됐을 때** |
| `changed` | `cold` 조건 + `inputs` 파일 내용이 지난번 성공 때와 다를 때 |

「지난번 성공」 은 warm 슬롯별로 작업 저장소에 기록한다(design §11.3).

### 9.5 `[[step.artifact]]`

| 키 | 타입 | 뜻 |
|---|---|---|
| `name` | 문자열 | 레시피 안에서 고유. 도구 호출의 `artifacts` 인자로 고른다 |
| `glob` | 문자열 또는 배열 | 작업 트리 기준 |
| `exclude` | 문자열 배열 | |
| `expect` | 정수 | 정확히 이 개수가 있어야 한다 |
| `expect_min` | 정수 | 최소 개수(`expect` 와 둘 중 하나) |
| `pack` | `"zip"` | 맞은 파일을 하나의 zip 으로 묶는다 |
| `pack_name` | 문자열 | zip 이름. 변수 확장 |
| `default` | 불리언 | 기본 `true`. 호출자가 `artifacts` 를 안 주면 기본값인 것만 수집 |
| `inspect` | 배열 | 수집 뒤 돌리는 검사(§9.7) |
| `description` | 문자열 | |

### 9.6 사전 정리 — 산출물과 판정 리포트

**요청된 산출물의 glob, 그리고 그 단계의 `junit` 판정기 `reports` glob 에 맞는 기존 파일을, 단계가 돌기 전에 지운다.**

- 산출물: 단계가 끝난 뒤 glob 에 맞는 파일은 그 단계가 만든 것이다. 같은 폴더에 쌓인 옛 버전이나 이전 작업의 파일을 새 산출물로 넘기지 않는다.
- 판정 리포트: 증분 빌드 도구가 테스트를 「이미 최신」 으로 건너뛰지 않고 다시 돌리거나 캐시에서 복원하게 한다. 옛 리포트를 인정하면 조용한 통과가 된다.
- 「단계 시작 뒤 수정된 파일만 인정」 은 쓰지 않는다 — 증분 빌드는 바뀌지 않은 파일을 다시 쓰지 않으므로 멀쩡한 빌드를 실패로 만든다.
- 비용(다시 링크·다시 번들·다시 테스트)은 **요청된 산출물과 그 단계의 리포트에만** 든다.
- **git 이 추적하는 파일은 지우지 않는다.** glob 이 추적 파일에 맞으면 그 단계는 `fail`/`recipe`(접수 검사, §15.2).
- 지운 파일 목록은 그 단계 로그 맨 앞에 남긴다.
- 산출물 개수가 `expect`·`expect_min` 과 다르면 그 단계는 `fail`/`code`.

### 9.7 `inspect` — 산출물 검사

```toml
inspect = ["apk-signer", { builtin = "pe-version", required = true }, { script = ["python", "${recipes}/scripts/check_manifest.py"] }]
```

| 형태 | 뜻 |
|---|---|
| `"<이름>"` | 내장 검사 — `"apk-signer"`·`"pe-version"`. `required = false` 와 같다 |
| `{ builtin = "<이름>", required = <bool> }` | 내장 검사 |
| `{ script = [<argv>], required = <bool> }` | 레시피 저장소의 검사 스크립트(빌드 계정). 환경에 `CIMON_ARTIFACT`(산출물 경로). stdout JSON 이 결과의 `inspect` 에 실린다 |

- 검사가 실패하면(스크립트가 죽음·JSON 이 틀림·내장 검사가 형식을 못 읽음): `required = true` 면 그 단계 `inconclusive`/`recipe`, 아니면 산출물에 `inspect_error` 를 적고 `warnings` 에 넣는다.
- 검사는 판정을 바꾸지 않는다 — 값이 틀렸다고 판단해야 하면 `script` 판정기(§12)를 쓴다.

---

## 10. 겹치기 — `extends` · `include`

**`extends`**(프리셋): 나열한 순서대로 겹치고, 마지막에 프로젝트 레시피를 겹친다.

| 대상 | 규칙 |
|---|---|
| `vars`·`env`·`toolchains`·`params`·`isolate`·`account_env` | 키 단위로 뒤가 앞을 바꾼다 |
| `workspace.keep` | 합친다 |
| `workspace.clean_env` | 키 단위로 뒤가 앞을 바꾼다 |
| `step` | **id 단위로 통째 교체.** 필드 단위로 섞지 않는다 — 결과를 읽기 쉽게. `disabled = true` 로 끈다 |
| `fixture` | 이름 단위로 통째 교체 |

**`include`**(조각): 같은 출처, 같은 디렉터리의 파일을 이어 붙인다. `step`·`fixture`·`params` 만 담을 수 있다. **include 사이, include 와 recipe 사이에 같은 id 가 있으면 오류다** — 교체는 `extends` 에서만 일어난다. 위 층이 `disabled` 로 끈 id 를 include 가 다시 정의해도 오류다. 프리셋·include 파일도 `schema = 1` 이어야 한다.

겹친 결과는 `cimon recipe show <project> --resolved` 와 MCP `describe_project` 로 본다. 리뷰의 의미 차이(review-ui §4.4)도 겹친 결과끼리 비교한다.

---

## 11. `[fixture.<name>]`

여러 단계가 공유하는 「떠 있어야 하는 것」(시뮬레이터 스택, 테스트 서버 등).

```toml
[fixture.sim]
after = ["build-sim", "build-cli"]
requires_artifacts = ["example-cli.exe"]     # 고정 설치본이 아니라 이 브랜치가 지은 CLI 로 스택을 띄운다
on_up_failure = "code"                       # 브랜치가 지은 CLI 가 띄우므로 기동 실패는 코드 문제
env = { SIM_INSTANCE = "cimon-${job.id}", SIM_CLI = "${artifact.example-cli.exe}" }
collect = [".runtime/cimon-${job.id}/logs/**"]

  [[fixture.sim.up]]
  run = ["${artifact.example-cli.exe}", "up", "--sim", "--instance", "cimon-${job.id}"]
  timeout_min = 3

  [fixture.sim.ready]
  run = ["${artifact.example-cli.exe}", "status", "--instance", "cimon-${job.id}"]
  until = { exit = 0 }
  every_s = 3
  timeout_s = 120
  on_timeout = { run = ["${artifact.example-cli.exe}", "status", "--instance", "cimon-${job.id}", "--format", "json"], exited_path = "$.services[?(@.state=='exited')].name" }

  [[fixture.sim.down]]
  run = ["${artifact.example-cli.exe}", "down", "--instance", "cimon-${job.id}"]
  timeout_min = 2

  [fixture.sim.gone]
  run = ["${artifact.example-cli.exe}", "status", "--instance", "cimon-${job.id}"]
  until = { exit_not = 0 }
```

| 키 | 뜻 |
|---|---|
| `after` | fixture 를 올리기 전에 끝나야 하는 단계(선택 집합에 자동으로 들어간다) |
| `requires_artifacts` | fixture 가 쓰는 산출물. 그 산출물과 생산 단계가 자동으로 요청된다. `${artifact.<name>}` 로 참조한다 |
| `env` | 이 fixture 를 쓰는 모든 단계에 들어가는 환경 변수(§7.4 의 층) |
| `up` | 순서대로 실행. 하나라도 실패하면 그 fixture 를 쓰는 단계 전부 `on_up_failure` 에 따라 판정 |
| `on_up_failure` | `"infra"`(기본) → `inconclusive`/`infra` · `"code"` → `fail`/`code`. **`up` 이 이 브랜치가 지은 산출물(`${artifact.*}`)을 실행하면 `"code"`** 로 둔다 — 브랜치가 CLI 를 깨뜨린 것을 「환경 문제, 재시도」 로 보이게 하지 않는다. `up` 이 `${artifact.*}` 를 쓰는데 이 키가 없으면 경고 |
| `ready` | `every_s` 마다 실행해 `until` 이 참이 될 때까지. `timeout_s` 안에 안 되면 `on_timeout` 으로 원인을 가린다 |
| `ready.on_timeout` | 명령을 실행하고, `exited_path` 가 가리키는 값(기동 중 종료된 구성 요소 이름)이 있으면 쓰는 단계 전부 **`fail`/`code`** 와 그 이름. 없으면 `inconclusive`/`infra`. 「브랜치가 스택을 깨뜨렸다」 와 「환경이 안 떴다」 를 가른다 |
| `until.exit` · `until.exit_not` | 종료 코드가 이 값인가 / 이 값이 아닌가 |
| `until.json_all` | stdout JSON 에서 JSONPath 가 가리키는 값이 **모두** `equals` 인가. 비어 있으면 거짓 |
| `collect` | `down` **전에** 이 glob 에 맞는 파일을 작업 로그로 복사한다(서비스 로그 등). 다음 작업의 `git clean` 이 지우기 전에 증거를 남긴다 |
| `down` | **무조건** 실행 — 실패·취소·시간 초과 뒤에도 |
| `gone` | `down` 뒤에 실행해 fixture 가 정말 사라졌는지 확인한다. 아니면 결과 `warnings` 에 「fixture 잔류」 를 남기고, 작업 끝의 빌드 계정 프로세스 정리가 마무리한다(design §9.2) |

- 수명: 그 fixture 를 쓰는 첫 단계 직전에 올리고, 마지막 단계 직후에 내린다. 작업당 한 번.
- fixture 를 쓰는 시나리오들은 같은 스택을 공유한다. 시나리오 사이의 상태 초기화는 시나리오 단계(또는 판정 스크립트의 출발 상태 확인)가 책임진다.
- fixture 가 예약 작업으로 프로세스를 띄우면 `[account_env]`(§7.3)로 홈을 맞춘다.

---

## 12. `judge` — 통과 판정

| type | 판정 | 필드 |
|---|---|---|
| `exit`(기본) | 종료 코드 0 이면 `pass`, 아니면 `fail`/`code` | — |
| `cargo-test` | 종료 코드 0, 그리고 출력의 `test result:` 줄을 합산해 `passed ≥ min_tests` | `min_tests`(기본 1) |
| `junit` | 종료 코드 0, 그리고 `reports` 에 맞는 JUnit XML(사전 정리 뒤 새로 생긴 것)을 합산해 failures·errors 가 0, tests ≥ `min_tests` | `reports`(glob), `min_tests`(기본 1) |
| `script` | 레시피 저장소의 판정 스크립트 결과 | `run`(argv — `${recipes}` 로 스크립트 경로) |

- **「테스트 0개 실행」 은 실패다.** `cargo-test`·`junit` 의 `min_tests` 가 그것을 잡는다.
- **`exit` 판정기는 0개 실행을 못 잡는다.** test·scenario 단계에 `exit` 를 쓰면 정적 검사가 경고한다. 개수를 셀 길이 없는 테스트(린트, 형식 검사, 개수를 출력하지 않는 러너)는 `uncounted = true` 로 알고 쓴다는 것을 밝힌다 — 경고가 사라지고 결과에 `uncounted: true` 가 실린다.
- **`script` 판정기의 약속**
  - 단계가 끝난 뒤, 빌드 계정으로 실행한다. 환경에 `CIMON_STEP_EXIT`(단계 종료 코드)·`CIMON_STEP_OUT`(`${step.out}`)이 들어간다.
  - stdout 에 JSON 한 개: `{"verdict": "pass|fail|inconclusive", "failure_class": "…", "reason": "…", "metrics": {…}}`. `verdict` 가 `pass` 가 아니면 `failure_class` 필수.
  - 허용 조합(design §8.2): `fail` 은 `code`·`recipe`, `inconclusive` 는 `recipe`·`environment`·`infra`·`unobserved`.
  - **종료 코드가 0 이 아니거나, JSON 이 없거나 틀리거나, 허용 조합 밖이면 `inconclusive`/`recipe`.** 판정기가 망가진 것을 통과로 넘기지 않는다.
  - `metrics` 는 결과의 그 단계에 실린다.
- 서비스에 특정 프로젝트의 결과 형식을 아는 판정기를 넣지 않는다. 그런 판정은 `script` 로 레시피 저장소에 둔다.

---

## 13. 변수

`${…}` 는 `run`·`cwd`·`env`·`glob`·`exclude`·`pack_name`·`inputs`·`workspace.keep`·`workspace.clean_env`·judge·inspect·fixture 의 명령과 `env`·`collect` 에서 확장된다. 모르는 변수는 검증 오류다. pre-clean·산출물 수집·JUnit 보고서·`inputs` 도 단계 명령과 같은 규칙으로 확장한다(`${ws}/x` 는 작업 트리 기준 `x`).

환경 변수 이름은 Windows 처럼 대소문자를 가리지 않는다 — 위 층의 `npm_config_cache` 는 아래 층의 `NPM_CONFIG_CACHE` 를 바꾼다.

| 변수 | 값 |
|---|---|
| `${ws}` | 작업 트리 루트(절대 경로) |
| `${recipes}` | 고정한 커밋의 레시피 저장소 체크아웃(읽기 전용) — 판정·검사 스크립트와 설정 파일 |
| `${vars.<name>}` | §4.1 |
| `${param.<name>}` | §8 — `applies_to` 단계에서만 |
| `${job.id}` | `j000123` |
| `${job.commit}` · `${job.commit_short}` | 빌드하는 커밋 |
| `${step.out}` | 그 단계 전용 출력 디렉터리. 판정용 파일을 여기 쓰게 한다 |
| `${artifact.<name>}` | 수집된 산출물의 경로(읽기 전용). 그 산출물이 요청돼 있어야 한다 — fixture 는 `requires_artifacts` 로 |
| `${toolchain.<kind>.home}` · `${toolchain.<kind>.bin}` | 갖춘 툴체인의 위치 |
| `${toolchain.<kind>.version}` | 정확한 버전 |
| `${sibling.<project>.root}` · `${sibling.<project>.commit}` | 형제 트리 |
| `${value}` | `params.*.args` 안에서만 — 그 파라미터 값 |

---

## 14. 선택과 순서

`build`·`test`·`run` 도구가 무엇을 돌릴지 정하는 규칙.

1. **후보**: `build` 는 kind `build`·`package`, `test` 는 `test`·`scenario`, `run` 은 네 종류 전부.
2. **고르기**: 호출자가 `steps`(id)·`tags` 를 주면 그 **합집합**, 아무것도 안 주면 후보 중 `default = true` 인 것(`run` 이면 기본 build + 기본 test).
   - **`artifacts` 로 고른 산출물의 생산 단계는 후보 종류와 상관없이 들어간다.**
   - 후보가 아닌 종류의 id 를 `steps` 에 주면 **거절**(「test 나 run 으로 부르라」).
   - 모르는 id·태그·산출물 이름은 **거절**.
3. **닫기**: `after`, fixture 의 `after`·`requires_artifacts` 를 재귀로 더한다. 순환은 레시피 오류.
4. **제외**: 러너에 없는 `needs` 를 가진 단계.
   - 호출자가 **id 로 직접 고른** 단계면 **거절**(「이 러너에는 display 가 없다」).
   - 기본값·태그로 들어온 단계면 **제외하고 결과 맨 앞에 적는다**(「제외 1: ui-test — display 필요」).
   - 제외된 단계에 `after` 로 기대는 단계도 제외된다(같은 이유를 적는다).
   - **제외 뒤 setup 말고 돌 단계가 하나도 없으면 거절한다**(`nothing_to_run`). 아무것도 안 돌고 통과하는 작업을 만들지 않는다.
5. **파라미터 적용 확인**: 호출자가 명시한 파라미터가 남은 단계 중 어디에도 적용되지 않으면 거절(`param_not_applicable`).
6. **setup**: 맨 앞에 레시피 순서대로, 각자의 `when` 에 따라.
7. **순서**: 위상 정렬, 같은 단계는 선언 순서(프리셋 먼저, 그다음 레시피, 그다음 include 순).
8. **실패 전파**:
   - `setup`·`build`·`package` 가 실패하면, 그것에 기대는 단계는 돌지 않고 「돌지 않음: <id> 실패」 로 남는다.
   - `test`·`scenario` 의 실패는 **다른 테스트를 멈추지 않는다** — 한 번에 실패를 전부 보기 위해서다.

`plan` 도구가 이 결과(돌 단계, 제외, 갖출 툴체인, 관리자 설치 필요 여부)를 실행 없이 보여 준다. `plan` 은 fetch 하지 않으므로 `version_file` 처럼 커밋을 읽어야 아는 것은 mirror 에 그 커밋이 없으면 「알 수 없음 — 접수 때 확인」 으로 적는다.

---

## 15. 검증 — 무엇이 오류인가

검사는 필요한 것에 따라 두 층이다(design §6.3).

### 15.1 정적 검사 — 레시피 저장소 내용만으로

`recipe_edit`·`recipe_sync`·`recipe_revert`·`cimon recipe import` 의 저장 전, 도구 호출의 입력 검증, 내보낸 폴더에 대한 로컬 `cimon recipe check` 가 같은 코드로 돈다. 저장 전 검사는 **이번 변경이 새로 만든 오류**만 저장을 막는다(design §6.3).

| 검사 | 예 |
|---|---|
| schema·모르는 키·타입 | `timeot_min` |
| 참조 | 모르는 프리셋, 카탈로그에 없는 툴체인 종류, 모르는 `after`·`fixture`·`uses`·`applies_to`·`requires_artifacts`, 등록되지 않은 형제 프로젝트 |
| 변수 | 모르는 `${…}`, 닫히지 않은 `${`, 값 없는(`default` 없는) 파라미터 참조, `applies_to` 밖에서 `${param.x}`, 요청되지 않을 수 있는 `${artifact.x}`, `[env]` 에서 단계마다 다른 값(`${param.*}`·`${artifact.*}`·`${step.out}`) |
| 파라미터 | `applies_to` 없음, 소비처(`env`·`args`·`${param.x}`) 없음 |
| 순환 | `after` 순환 |
| 중복 | include 끼리 같은 step id, 산출물 이름 중복 |
| 겹치기 | 앞 층에 없는 id 를 끄는 `disabled`, include 의 `disabled`·위 층이 끈 id 재정의, 프리셋·include 의 `schema` |
| 경로 | `cwd`·`glob` 이 작업 트리 밖, 절대 경로, `${recipes}` 밖을 가리키는 스크립트 경로, **그 레시피 커밋에 없는 `${recipes}/…` 파일**(확장되는 모든 문자열의 모든 참조, `/`·`\` 둘 다), Windows 에서 다른 파일이 되는 경로(§1) |
| 버전 | managed 툴체인에 범위·채널 이름 |
| 환경 | `[account_env]` 의 이름이 `[isolate]` 에 없음 |
| 산출물 | `expect`·`expect_min` 이 둘 다 있거나 둘 다 없음, setup·test·scenario 에 산출물 |
| 판정 | `junit` 에 `reports` 없음, `script` 에 `run` 없음 |
| 정규식 | `pattern`·`error_patterns` 컴파일 실패 |
| 카탈로그 | `probe.expect` 에 `{version}`·`{version_probe}` 없음(base `version_any` 는 반대로 자리표시자가 있으면 오류), managed 에 `version_any`, 체크섬 출처 없는 managed, `script` 설치 방식의 `ref` 가 전체 커밋 SHA 가 아님, `from_project` 가 등록되지 않은 프로젝트 |
| **경고**(저장을 막지 않음) | `[env]` 에 `CARGO_TARGET_DIR`·`RUSTFLAGS` · `default = true` 인 단계가 하나도 없음 · `fail_on_output` 이 하나도 없는 형제 저장소 의존 단계 · test·scenario 에 `exit` 판정기인데 `uncounted` 없음 · 위 층이 아래 층 값을 담지 않고 환경 키를 바꿈(§7.4) · `up` 이 `${artifact.*}` 를 쓰는데 `on_up_failure` 없음 |

### 15.2 접수 검사 — 프로젝트 저장소의 그 커밋과 서버 설정이 있어야

작업의 접수 단계에서만 돈다. 로컬 `cimon recipe check` 는 이 검사를 「건너뜀 — 서버·저장소 필요」 로 명시한다. 실패하면 작업이 그 이유 코드와 함께 끝난다.

| 검사 | 실패하면 |
|---|---|
| `version_file` 이 그 커밋에 있고, 정확한 버전이고, 저장소 안의 다른 `rust-toolchain.toml` 과 같은가 | `fail`/`recipe`(`version_file_missing`·`version_file_mismatch`) |
| 요청된 산출물 glob·`junit` `reports` glob 이 git 추적 파일에 맞지 않는가 | `fail`/`recipe`(`glob_matches_tracked`) |
| base 툴체인 — `vswhere`·`path_from`(`cimon.toml [base]`)·`sdkmanager` | `inconclusive`/`environment`(`base_missing`, 빠진 목록과 `hint`) |
| managed 툴체인이 설치돼 있거나 자동 설치가 허용되는가, 시도 브랜치의 카탈로그 항목이 설치돼 있는가(§16.5) | `inconclusive`/`environment`(`toolchain_not_provisioned`) |
| `recipe_source = "project"` 의 `.cimon/recipe.toml` 정적 검사(2단계) | `fail`/`recipe` |

---

## 16. 툴체인 카탈로그 — `toolchains.toml`

```toml
schema = 1

[kind.node]
tier = "managed"
installer = "archive"
url = "https://nodejs.org/dist/v{version}/node-v{version}-win-x64.zip"
checksum = { source = "shasums", url = "https://nodejs.org/dist/v{version}/SHASUMS256.txt" }
bin = ["node-v{version}-win-x64"]
probe = { run = ["node", "--version"], expect = "v{version}" }
```

### 16.1 공통 필드

| 필드 | 뜻 |
|---|---|
| `tier` | `managed`(서비스가 설치) · `base`(관리자가 설치, 서비스는 검사만) |
| `installer` | §16.2 |
| `bin` | 설치 루트 기준, `PATH` 에 넣을 디렉터리. `rustup`·`uv-python`·`uv-tool` 은 설치 방식이 정하므로 생략한다 |
| `home` | 설치 루트 기준, `${toolchain.<kind>.home}` 이 가리킬 곳(기본 설치 루트) |
| `exports` | 단계 환경에 넣을 변수. 예: `{ JAVA_HOME = "{home}" }` |
| `probe` | 설치 뒤·작업마다 **빌드 계정으로** 실행. `expect` 가 출력(stdout+stderr)에 들어 있어야 한다. **`expect` 에는 `{version}` 이나 `{version_probe}` 가 들어가야 한다** — 아무 버전이나 맞는 패턴은 검증 오류. 도구가 버전을 다르게 표기하면 `{version_probe}` 를 쓰고 `probe.version_map`(예: `{ strip_after = "+" }` → `17.0.19+10` 을 `17.0.19` 로)으로 바꾼다 |
| `version_any` | **base 전용.** `true` 면 버전을 고정하지 않고 존재만 확인한다. 이때 `expect` 는 **자리표시자 없는 고정 문자열**(예: `"GNU bash, version"`)이고, probe 출력은 결과·provenance 에 기록한다. managed 에는 쓸 수 없다 |
| `fields` | 레시피가 줄 수 있는 추가 필드 이름(예: `["components"]`) |
| `hint` | base 가 빠졌을 때 결과에 실을 설치 방법 |

자리표시자는 `{version}`·`{version_url}`(URL 인코딩)·`{root}`(설치 루트)·`{home}`·`{bin}`(첫 번째 bin)과 `fields` 에 선언한 이름. 레시피 변수(`${…}`)와 문법이 다르다.

**항목 해시**: 한 종류의 `[kind.<name>]` 표를 정규화(키 정렬)한 sha256. 설치 폴더 `toolchains\<kind>\<version>-<해시 8자>\` 와 설치 기록이 `(kind, version, 항목 해시)` 로 구분된다 — 같은 버전이라도 받는 곳·검증 방법이 바뀌면 다른 설치다.

### 16.2 설치 방식(내장, 새 방식 = 코드 변경)

**서비스 계정은 받은 것을 실행하지 않는다**(design §10.3). 다운로드·체크섬 확인·배치는 서비스 계정이, 스크립트·probe 실행은 빌드 계정이 `toolchains\_staging\` 에서 한다.

| installer | 하는 일 | 필수 필드 | 체크섬 |
|---|---|---|---|
| `archive` | URL 의 zip 을 받아 staging 에 푼다(경로 탈출 검사) | `url`, `checksum` | `checksum.source`: `shasums`(체크섬 목록 파일) · `sha256`(카탈로그에 버전별 고정, `checksum.values = { "<ver>" = "<hex>" }`) · `adoptium`(API 응답) |
| `github-release` | GitHub release asset 을 받아 푼다 | `repo`, `tag`, `asset` | release API 의 asset `digest`(sha256). 없으면 설치 거절 |
| `rustup` | `rustup toolchain install {version} --profile minimal -c …`. PATH 에는 rustup 프록시 디렉터리 | `components` | rustup 이 검증 |
| `uv-python` | `uv python install {version}`. 레시피가 `packages` 를 주면 그 조합의 가상 환경을 만들고(`--no-build` — 휠만) 그 `Scripts` 를 PATH 에 | — | uv(PyPI 해시) |
| `uv-tool` | `uv tool install --no-build {package}=={version}` 을 버전별 디렉터리로 — **휠만**. 휠이 없는 패키지는 설치 실패(빌드 스크립트가 서비스 계정으로 돌지 않게) | `package` | PyPI 해시 |
| `script` | 등록된 프로젝트 mirror 에 있는 설치 스크립트를 **빌드 계정으로** staging 에서 실행: `script = { from_project = "<id>", ref = "<전체 커밋 SHA>", path = "<경로>", args = [...] }`(`args` 에 `{version}`·`{root}` — `{root}` 는 staging 경로) | `script` | 스크립트가 받은 파일을 검증해야 한다. 검증하지 않는 스크립트는 쓰지 않는다 |
| `base` | 설치하지 않는다. 검사만 | `check` 또는 `probe` | — |

- **체크섬 출처가 없는 managed 도구는 설치하지 않는다.**
- `script.ref` 는 **전체 커밋 SHA(40자)** 다. 브랜치 이름이면 그 브랜치에 push 할 수 있는 사람이 설치 내용을 바꿀 수 있다. 도구 버전을 올릴 때 SHA 도 함께 올리고, 그 변경은 카탈로그 변경이므로 반영 조건을 거친다(design §6.5).

### 16.3 base 검사

| `check` | 뜻 |
|---|---|
| `{ vswhere = true }` | `vswhere -products * -requires <components> -version [<min>,)` |
| `{ path_from = "<cimon.toml [base] 키>" }` | 그 경로에 `probe` 실행 |
| `{ sdkmanager = true }` | 레시피의 `platforms`·`build_tools` 가 SDK 안(`platforms\<p>`, `build-tools\<v>`)에 설치돼 있나 — `sdkmanager` 를 실행하지 않고 디렉터리로 확인한다(sdkmanager 는 JDK 가 필요하다) |

빠진 것이 있으면 작업은 접수에서 `inconclusive`/`environment` 로 끝나고, **빠진 목록과 `hint` 를 결과에 적는다.**

### 16.4 자동 설치

`cimon.toml [toolchains].auto_approve` 에 든 종류만 접수 단계에서 설치한다. 나머지는 관리자 `cimon provision` 으로 미리 설치한다(design §10.4).

### 16.5 시도 브랜치와 카탈로그

**설치는 `main` 카탈로그 항목으로만** 한다. 시도 브랜치(`recipe_ref`)로 돈 작업에서:

| 시도 브랜치의 그 종류 항목 | 동작 |
|---|---|
| `main` 과 같다(항목 해시가 같다) | 평소대로 — 설치돼 있으면 쓰고, 없으면 auto_approve 에 따라 `main` 항목으로 설치 |
| `main` 과 다르고, 그 항목 해시로 설치된 것이 있다 | 그것을 쓴다(관리자가 `cimon provision --recipe-ref` 로 설치한 것) |
| `main` 과 다르고, 설치된 것이 없다 | 접수에서 `inconclusive`/`environment`(`toolchain_not_provisioned`): 「시도 브랜치의 카탈로그 항목 `node` 가 main 과 다르다 — 관리자가 검토 후 `cimon provision --recipe-ref <branch> --kind node`」 |
| `main` 에 없는 새 종류 | 위와 같다 |

시도 브랜치를 만들 수 있는 사람(팀원 누구나)이 서비스 계정·빌드 머신에 설치될 내용을 바꾸지 못하게 하기 위해서다.

---

## 17. 프리셋 — `presets/<name>.toml`

레시피와 같은 형식이되 `project.toml` 성격의 키가 없다(`schema`, `description`, `vars`, `toolchains`, `env`, `isolate`, `account_env`, `params`, `step`, `fixture`, `workspace`). 프로젝트는 프리셋의 `vars` 기본값을 덮어써 경로 등을 맞춘다.

프리셋은 레시피 저장소에 둔다. 흔한 것: `rust-cargo`(build·test·clippy·fmt), `node-npm`(ci·build·test·lint), `dotnet`(restore·build·test). 린트·형식 검사처럼 개수를 셀 수 없는 test 단계는 프리셋에서 `uncounted = true` 로 둔다.
