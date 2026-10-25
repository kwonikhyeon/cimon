# 레시피 리뷰 화면 설계

- 상태: 초안 · 2026-10-26 — 구현 전
- 서비스 전체 구조는 [design.md](design.md), 레시피 형식은 [recipe-spec.md](recipe-spec.md). 이 문서는 레시피 변경을 **사람이 보고 반영하는 웹 화면**이다.

---

## 1. 무엇을 하나

레시피는 빌드 머신 안 git 저장소에 있고 외부 원격이 없다(design §6.4). 그래서 GitHub PR 화면 같은 리뷰 수단이 따로 필요하다. 이 화면이 그것이다.

| 한다 | 하지 않는다 — 이유 |
|---|---|
| 제안(시도 브랜치) 목록과 상세 | **레시피 편집** — 편집은 Claude 의 `recipe_edit` 로만 한다. 화면에 편집기를 두면 저장 전 검사·충돌 처리를 두 곳에서 해야 한다 |
| **의미 차이**(단계·툴체인·파라미터·권한이 어떻게 바뀌나)와 파일 차이 | **빌드 시작** — 시도 실행은 MCP 로 건다. 화면은 그 결과를 보여 준다 |
| 반영 조건(검사·병합 가능·시도 실행)과 그 상태 | 외부 공개 — tailnet 안에서만, 로그인한 팀원만 |
| 토론(제안에 댓글) — Claude 가 MCP 로 읽고 반영할 수 있다 | 알림(메신저 등) — 1단계 밖(§15) |
| 반영·닫기·되돌리기(권한별) | |
| 레시피 탐색(파일, 겹친 결과), 이력 | |
| 작업 상세 보기(읽기) — 시도 실행 결과로 이어지는 곳 | |

---

## 2. 개념 — 제안(proposal)

**제안 = `recipe/*` 브랜치 + 제목·설명·상태.** GitHub 의 PR 에 해당한다.

```
            recipe_edit(새 브랜치) 또는 recipe_propose
                         |
                         v
                      [open] ----- 닫기(작성자·관리자) -----> [closed]
                         |
          반영(관리자, 반영 조건 충족 또는 사유를 적은 덮어쓰기)
                         |
                         v
                    [promoted]
```

- 새 `recipe/*` 브랜치에 첫 `recipe_edit` 이 일어나면 제안이 **자동으로 열린다**(제목 = 첫 커밋 메시지). `recipe_propose` 로 제목·설명을 고칠 수 있다.
- 반영되면 브랜치는 남기되 읽기 전용이 된다(이력에서 「어느 제안으로 들어왔나」 를 잇기 위해). 닫힌 제안의 브랜치는 30일 뒤 정리한다.
- 반영 안 된 채 30일 동안 아무 변화가 없으면 자동으로 닫고 그 사실을 토론에 남긴다(`trial_branch_idle_days`).

---

## 3. 화면 목록

모든 경로는 `https://cimon.<tailnet>.ts.net/ui/…` 아래다.

| 경로 | 화면 | 주 사용자 |
|---|---|---|
| `/ui/` | 첫 화면 — 열린 제안, 최근 반영, 최근 작업 | 모두 |
| `/ui/proposals` | 제안 목록(열림·반영됨·닫힘 필터) | 모두 |
| `/ui/proposals/<branch>` | **제안 상세** — 반영 조건, 의미 차이, 파일 차이, 시도 실행, 토론 | 리뷰어·관리자 |
| `/ui/recipes/<ref>/tree/<path>` | 파일 보기(브랜치·커밋 기준) | 모두 |
| `/ui/recipes/<ref>/projects/<id>` | 겹친 레시피 — `describe_project` 의 웹판 | 모두 |
| `/ui/history/<ref>` | 커밋 이력, 커밋별 차이, 되돌리기(관리자, `main` 만) | 관리자 |
| `/ui/jobs/<job>` | 작업 상세(읽기) — 판정, 단계, 로그 끝, 산출물 링크 | 모두 |
| `/ui/login` | 일회용 링크로 들어오는 곳(§8) | — |

---

## 4. 제안 상세 화면

### 4.1 생김새

```
+--------------------------------------------------------------------------------------+
| cimon › 제안 › recipe/add-desktop-test                               admin-a (관리자) |
+--------------------------------------------------------------------------------------+
| desktopTest 단계 추가                                                     [open]      |
| 작성 member-b · 3시간 전 · head 7c1e9a2 · 기준 main a1b2c3d                          |
|                                                                                      |
| 반영 조건                                                                            |
|   [통과] 레시피 검사 — head 7c1e9a2 기준                                              |
|   [통과] main 에 바로 반영 가능(fast-forward)                                         |
|   [통과] 시도 실행 — example-app: j000214 pass (head 7c1e9a2)                         |
|   [ - ]  다른 관리자의 반영 — 설정상 필요 없음                                        |
|                                                     [ 반영 ]  [ 닫기 ]               |
+--------------------------------------------------------------------------------------+
| [의미 차이]  [파일 차이 (1)]  [시도 실행 (2)]  [토론 (3)]                              |
+--------------------------------------------------------------------------------------+
| 영향받는 프로젝트: example-app                                                        |
|                                                                                      |
| example-app                                                                          |
|   단계 추가  test-shared-desktop  (test, 기본 아님)                    [실행]          |
|     run   ./gradlew.bat :shared:desktopTest --console=plain                           |
|     cwd   app                                                                         |
|     judge junit  reports=app/shared/build/test-results/desktopTest/*.xml              |
|           min_tests=1                                                                |
+--------------------------------------------------------------------------------------+
```

### 4.2 머리

- 제목·상태·작성자·**head 커밋**(지금 보고 있는 내용)·**기준 커밋**(갈라져 나온 `main`).
- head 가 바뀌면(누가 그 사이 `recipe_edit`) 화면 위에 「이 제안은 7c1e9a2 → 9f3b… 로 바뀌었다 — 새로 고침」 띠를 띄우고 반영 버튼을 막는다(§4.7).

### 4.3 반영 조건

| 조건 | 판정 | 기본 |
|---|---|---|
| **레시피 검사** | head 에서 레시피 저장소 전체 검사(recipe-spec §15) | 필수 |
| **병합 가능** | `main` 이 기준 커밋 그대로면 fast-forward. 움직였으면 메모리 안 3-way 병합을 시험 — 충돌 없으면 「자동 병합 가능(반영 때 병합 커밋 + `main` 기준 재검사)」, 충돌이면 파일 목록 | 필수 |
| **시도 실행** | 의미 차이가 영향을 준 프로젝트마다, **head 커밋으로 돈** 시도 작업이 있고 판정이 `pass` 인가 | `cimon.toml [review].require_trial`, `trial_must_pass` |
| **다른 관리자** | 반영하는 관리자가 작성자가 아닌가 | `require_other_admin`(기본 꺼짐 — 팀이 작다) |

- 각 조건은 **통과 · 실패 · 계산 중 · 계산 실패 · 이전 커밋 기준** 다섯 상태 중 하나다. 「계산 실패」 와 「이전 커밋 기준」 은 통과로 보이지 않는다(§13).
- 충돌일 때는 해결 방법을 같이 보여 준다: 「Claude 에게 『recipe/add-desktop-test 를 main 에 맞춰 줘』 라고 요청 — `recipe_sync` 가 충돌 없는 부분을 합치고, 충돌 파일은 다시 고치게 한다」.
- 시도 실행이 없거나 실패했으면 그 프로젝트와 필요한 호출을 보여 준다: 「`test(project="example-app", recipe_ref="recipe/add-desktop-test")`」.

### 4.4 의미 차이

**파일 차이만으로는 「무엇이 실행되게 바뀌나」 를 읽기 어렵다.** 프리셋·include·카탈로그를 겹친 결과를 `main` 과 head 에서 각각 만들어 비교한다(`cimon-recipe` 의 겹치기 그대로).

1. **영향받는 프로젝트를 정한다**: 바뀐 파일이 `projects/<id>/` 아래면 그 프로젝트. 프리셋·카탈로그·공용 스크립트가 바뀌면 그것을 쓰는 모든 프로젝트.
2. 프로젝트마다 겹친 레시피를 비교한다.
   - 단계: id 기준으로 추가·삭제·변경, 변경이면 필드별(`run`·`cwd`·`env`·`judge`·`after`·`default`·`needs`·`timeout_min`·산출물 …)
   - 툴체인 요구, 파라미터, fixture, 작업 공간(`keep`·`clean_env`), `[env]`·`[isolate]`
   - `project.toml`
3. 카탈로그 차이: 종류 추가·삭제, 설치 방식·URL·체크섬 출처·스크립트 변경.
4. 스크립트 파일 차이: 어떤 단계가 그 스크립트를 쓰는지 함께.

**위험 표시** — 리뷰어가 먼저 볼 것을 표시한다.

| 표시 | 언제 | 왜 |
|---|---|---|
| **[권한]** | `project.toml` 의 `repo`·`allowed_refs`·`[[sibling]]` 변경, 새 프로젝트 | 반영되면 빌드할 수 있는 저장소·브랜치가 바뀐다(design 원칙 7) |
| **[실행]** | `run`·`shell`·`cwd`·`env`, 판정·검사 스크립트의 명령과 **파일 내용**, fixture 명령의 추가·변경 | 빌드 머신에서 실행되는 명령이 바뀐다 |
| **[공급망]** | 카탈로그의 URL·체크섬 출처·설치 방식·설치 스크립트 변경, 새 종류 | 받아 오는 바이너리가 바뀐다 |
| **[동작]** | `default`·`needs`·`tags`·`timeout_min`·`fail_on_output`·판정 기준 변경 | 무엇이 돌고 무엇을 통과로 칠지가 바뀐다 — 특히 판정을 느슨하게 하는 변경(`min_tests` 감소, `fail_on_output` 삭제)은 **[동작·완화]** 로 따로 |

의미 차이를 계산하지 못하면(겹치기 실패 등) 빈 화면이 아니라 「의미 차이 계산 실패: <오류> — 파일 차이를 보라」 를 띄운다.

### 4.5 파일 차이

- 파일별 unified diff, 줄 번호, TOML·Python 구문 강조(서버에서). 좌우 보기 전환.
- 파일 목록에 위험 표시를 같이 붙인다.
- 줄을 눌러 그 줄에 대한 토론을 남길 수 있다(§4.6).

### 4.6 시도 실행 · 토론

**시도 실행 탭**: 이 브랜치로 돈 작업 목록 — 작업 번호, 프로젝트, 도구·선택, 판정·실패 분류, **어느 head 로 돌았나**. 현재 head 가 아닌 것은 흐리게 「이전 커밋」. 작업 번호는 `/ui/jobs/<job>` 로 이어진다.

**토론 탭**: 제안 단위 댓글, 선택적으로 파일·줄을 가리킨다. 댓글에는 그때의 head 커밋이 기록돼, 이후 바뀐 줄에 남은 댓글은 「이전 커밋에 단 댓글」 로 보인다.

- Claude 는 `recipe_proposal` 로 댓글을 읽을 수 있다 — 「리뷰 댓글 반영해 줘」 → 읽고 `recipe_edit` → 새 head → 반영 조건 다시 계산.
- 댓글은 Markdown 을 **글자 그대로**(굵게·코드 정도만) 보여 준다. HTML 은 쓰지 못한다(§10.3).

### 4.7 반영 · 닫기

- **반영**(관리자): 버튼은 화면이 보여 준 **head 커밋을 함께 보낸다.** 서버는 반영 순간에 모든 조건을 **다시 계산**하고(화면의 상태를 믿지 않는다), head 가 다르면 「제안이 그 사이 바뀌었다 — 다시 보라」 로 거절한다. 리뷰어가 본 것과 다른 내용이 반영되는 일을 막는다.
  - fast-forward 가 되면 그대로, 아니면 병합 커밋을 만들고 `main` 기준으로 재검사 — 실패하면 반영하지 않는다.
  - 반영되면 제안 `promoted`, 반영 커밋·반영자 기록, 다음 작업부터 쓰인다.
- **조건을 덮어쓰는 반영**: 시도 실행이 없거나 실패했는데 반영해야 할 때(예: 카탈로그 URL 오타 수정) 관리자는 **사유를 적어야** 반영할 수 있다. 사유는 제안·이력·감사 로그에 남고, 이력 화면에 「조건 덮어씀」 표시가 붙는다. 레시피 검사·병합 가능은 덮어쓸 수 없다.
- **닫기**(작성자·관리자): 사유 선택 입력. 브랜치는 남아 30일 뒤 정리.

---

## 5. 이력 · 되돌리기

- `main`(또는 브랜치)의 커밋 목록: 작성자·시각·메시지·**어느 제안으로 들어왔나**·조건 덮어씀 여부.
- 커밋을 누르면 그 커밋의 의미 차이·파일 차이.
- **되돌리기**(관리자, `main` 만): 되돌리는 커밋을 미리 보여 주고(의미 차이), 확인하면 `recipe_revert` 와 같은 동작. 되돌린 결과도 레시피 검사를 통과해야 한다.

---

## 6. 레시피 탐색

- **파일 보기**: 브랜치·커밋 선택, 디렉터리 트리, 파일 내용(구문 강조), 그 파일의 이력.
- **겹친 레시피**: 프로젝트 하나의 단계 표(id·종류·태그·기본·needs·after·판정·산출물), 툴체인, 파라미터, fixture. 각 항목이 어느 파일(프리셋·include)에서 왔는지 표시.

---

## 7. 작업 상세(읽기)

`job_status` 의 웹판. 판정·실패 분류·요약, 커밋·레시피 커밋·시도 여부, 제외된 단계, 단계별 결과·시간·판정 지표, 단계 로그 끝 200줄(ANSI 제어 문자 제거), 산출물 목록과 다운로드 링크(새로 발급), `provenance.json`. MCP 결과에도 이 화면의 주소(`job_url`)를 싣는다.

---

## 8. 로그인과 권한

### 8.1 일회용 링크로 로그인

브라우저에 Bearer 토큰을 넣게 하지 않는다. 대신:

1. Claude 에게 「리뷰 화면 열어 줘」 → MCP `web_login` 이 **5분짜리 일회용 링크**를 돌려준다(`https://cimon.<tailnet>.ts.net/ui/login?code=…`). 링크는 그 토큰의 사용자에 묶인다.
2. 링크를 열면 세션 쿠키가 생기고 코드는 바로 소모된다.
3. 제안·작업 링크를 로그인 없이 열면 「Claude 에게 리뷰 화면 로그인 링크를 요청하라」 와 토큰 입력 칸(대체 경로)을 보여 준다.

`recipe_edit`·`recipe_propose` 결과에도 제안 화면 주소를 싣는다 — Claude 가 「리뷰는 여기서」 를 바로 안내할 수 있게.

Tailscale 신원 헤더는 여기서도 **인증에 쓰지 않는다** — 빌드 머신의 로컬 프로세스가 `127.0.0.1` 로 직접 들어오며 헤더를 꾸밀 수 있다(design §15.2).

### 8.2 세션

| 항목 | 값 |
|---|---|
| 쿠키 | `HttpOnly` · `Secure` · `SameSite=Strict` · 경로 `/ui` |
| 수명 | 12시간(`[web].session_hours`), 로그인할 때마다 새 세션 id |
| 저장 | 세션 id 의 해시만 DB 에 |
| 끊기 | 토큰을 끄면(`users.disabled_at`) 그 사용자의 세션도 끊긴다 |

### 8.3 권한

| 동작 | 팀원 | 관리자 |
|---|---|---|
| 모든 화면 보기 | ● | ● |
| 댓글 | ● | ● |
| 자기 제안 닫기 | ● | ● |
| 남의 제안 닫기 | | ● |
| 반영(조건 충족) | | ● |
| 반영(조건 덮어쓰기, 사유 필수) | | ● |
| 되돌리기 | | ● |

---

## 9. MCP 도구 연동

리뷰 화면과 같은 데이터를 Claude 도 쓴다. design §7.2 의 도구에 더한다.

| 도구 | 하는 일 | 권한 |
|---|---|---|
| `recipe_propose` | 제안의 제목·설명을 쓰거나 고친다(없으면 연다). 결과에 제안 화면 주소 | 팀원 |
| `recipe_proposals` | 제안 목록(상태 필터) | 팀원 |
| `recipe_proposal` | 제안 하나 — 반영 조건 상태, 의미 차이 요약, 시도 실행, **댓글** | 팀원 |
| `recipe_comment` | 댓글을 단다(Claude 가 「이렇게 고쳤다」 를 남길 때) | 팀원 |
| `recipe_sync` | `main` 을 그 브랜치에 합친다 — 충돌이 없을 때만, 합친 뒤 검사. 충돌이면 파일 목록을 돌려준다 | 팀원 |
| `web_login` | 5분짜리 일회용 로그인 링크 | 팀원 |

`recipe_promote` 는 화면의 반영과 같은 조건(§4.3)과 같은 head 고정(§4.7)을 따른다 — MCP 로 반영해도 우회되지 않는다. 덮어쓰기는 `override_reason` 을 받는다.

---

## 10. 구현

### 10.1 구성

| 부분 | 선택 | 이유 |
|---|---|---|
| 서버 | 같은 `cimon.exe`, 같은 `127.0.0.1:8770` 의 `/ui/*` 경로(axum) | 배포 단위를 늘리지 않는다. `tailscale serve` 가 그대로 내보낸다 |
| 화면 | **서버 렌더링 HTML**(maud — 기본이 이스케이프), 상호작용은 작은 JS(htmx 수준, 실행 파일에 포함) | 프런트 빌드 파이프라인을 따로 두지 않는다. JS 가 꺼져도 폼 POST 로 동작 |
| 정적 자원 | 실행 파일에 포함(rust-embed). **외부 CDN 없음** | tailnet 안에서만 돈다. 외부 의존 없이 |
| 차이 계산 | `similar` 크레이트(파일 차이), `cimon-recipe` 의 겹치기 결과 비교(의미 차이) | 의미 차이는 레시피 검사와 같은 코드로 |
| 구문 강조 | 서버에서 TOML·Python 을 간단히 토큰 분리해 `<span class>` | 클라이언트 라이브러리 없이 |
| 테마 | CSS 변수로 밝은·어두운(`prefers-color-scheme`) | |

코드는 새 크레이트 `cimon-web` 에 둔다(라우트·템플릿·자원). 반영 조건 계산·의미 차이는 `cimon-recipestore` 에 둬서 화면과 MCP 가 같은 함수를 부른다.

### 10.2 반영 조건 계산

- 제안 head 가 바뀔 때(편집·`recipe_sync`)와 `main` 이 움직일 때 다시 계산해 `proposal_checks` 에 **head 커밋별로** 저장한다.
- 시도 실행 조건은 작업이 끝날 때마다 갱신한다.
- 계산 중이면 화면은 「계산 중」, 실패하면 「계산 실패 + 오류」.

### 10.3 보안

| 위협 | 대응 |
|---|---|
| XSS — 레시피 파일·스크립트·댓글·로그는 모두 사용자 입력이다 | 템플릿 기본 이스케이프, 댓글 Markdown 은 허용 목록(굵게·기울임·코드·링크 텍스트)만, **CSP** `default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; frame-ancestors 'none'; base-uri 'none'`, 인라인 스크립트 없음 |
| CSRF | `SameSite=Strict` + 폼마다 세션별 CSRF 토큰. 상태를 바꾸는 것은 POST 만 |
| 클릭재킹 | `frame-ancestors 'none'` |
| 로그인 코드 탈취 | 5분, 일회용, 사용자에 묶임, 해시만 저장. 로그인 시도 빈도 제한 |
| 리뷰어가 본 것과 다른 것이 반영됨 | 반영 요청에 head 커밋, 서버가 재계산(§4.7) |
| 권한 우회 | 모든 동작을 서버에서 역할로 검사 — 화면에서 버튼을 숨기는 것은 편의일 뿐 |

---

## 11. 데이터 모델 추가

```sql
CREATE TABLE proposals (
  branch          TEXT PRIMARY KEY,              -- recipe/<설명>
  title           TEXT NOT NULL,
  description     TEXT,
  author          TEXT NOT NULL,
  status          TEXT NOT NULL,                 -- open | promoted | closed
  base_commit     TEXT NOT NULL,                 -- 갈라져 나온 main
  head_commit     TEXT NOT NULL,
  created_at      TEXT NOT NULL,
  updated_at      TEXT NOT NULL,
  promoted_commit TEXT, promoted_by TEXT, promoted_at TEXT,
  override_reason TEXT,                          -- 조건을 덮어쓴 반영의 사유
  closed_by TEXT, closed_at TEXT, close_reason TEXT
);

CREATE TABLE proposal_checks (
  branch           TEXT NOT NULL REFERENCES proposals(branch),
  head_commit      TEXT NOT NULL,
  main_commit      TEXT NOT NULL,                -- 계산할 때의 main
  computed_at      TEXT NOT NULL,
  validation       TEXT NOT NULL,                -- pass | fail | error
  validation_json  TEXT,
  merge_state      TEXT NOT NULL,                -- fast_forward | auto_merge | conflict | error
  conflicts_json   TEXT,
  affected_json    TEXT,                         -- 영향받는 프로젝트
  semantic_json    TEXT,                         -- 의미 차이(위험 표시 포함)
  PRIMARY KEY (branch, head_commit, main_commit)
);

CREATE TABLE proposal_comments (
  id          INTEGER PRIMARY KEY,
  branch      TEXT NOT NULL REFERENCES proposals(branch),
  author      TEXT NOT NULL,
  body        TEXT NOT NULL,
  file        TEXT, line INTEGER,                -- 선택: 가리키는 줄
  head_commit TEXT NOT NULL,                     -- 댓글을 단 때의 head
  created_at  TEXT NOT NULL, edited_at TEXT, deleted_at TEXT
);

CREATE TABLE web_sessions (
  id_sha256    TEXT PRIMARY KEY,
  user         TEXT NOT NULL,
  csrf         TEXT NOT NULL,
  created_at   TEXT NOT NULL,
  expires_at   TEXT NOT NULL,
  last_seen_at TEXT,
  revoked_at   TEXT
);

CREATE TABLE login_codes (
  code_sha256 TEXT PRIMARY KEY,
  user        TEXT NOT NULL,
  created_at  TEXT NOT NULL,
  expires_at  TEXT NOT NULL,
  used_at     TEXT
);
```

시도 실행 조건은 `jobs`(`trial = 1`, `recipe_commit = head`, 프로젝트)에서 계산한다 — 따로 저장하지 않는다. `audit.action` 에 `proposal_open`·`proposal_close`·`comment`·`web_login` 을 더한다.

---

## 12. 설정 추가 — `cimon.toml`

```toml
[web]
enabled = true
session_hours = 12
login_code_minutes = 5

[review]
require_trial = "changed_projects"     # changed_projects | none
trial_must_pass = true                 # 시도 실행 판정이 pass 여야(아니면 사유를 적은 덮어쓰기)
require_other_admin = false            # 작성자가 아닌 관리자만 반영
```

---

## 13. 화면에서도 실패는 실패로 보인다

| 상황 | 화면 |
|---|---|
| 반영 조건 계산이 실패했다 | 「계산 실패」(빨강) + 오류. 통과로 보이지 않는다. 반영 버튼 막힘 |
| 계산 결과가 지금 head·`main` 기준이 아니다 | 「이전 커밋 기준」(회색) + 다시 계산. 반영 버튼 막힘 |
| head 가 화면을 연 뒤 바뀌었다 | 위쪽 띠 + 반영 버튼 막힘. 서버도 거절(§4.7) |
| 의미 차이를 만들지 못했다 | 「의미 차이 계산 실패」 — 빈 차이로 보이지 않는다 |
| 바뀐 것이 없다 | 「변경 없음」, 반영 버튼 막힘 |
| 시도 실행이 `inconclusive` | 통과로 치지 않는다(`trial_must_pass`). 실패 분류를 함께 |
| 판정을 느슨하게 하는 변경 | **[동작·완화]** 표시를 맨 위에 |
| 조건을 덮어쓴 반영 | 제안·이력에 사유와 함께 영구 표시 |

---

## 14. 완료 기준 — M9

설계 문서의 다른 단계처럼 실패 경로를 포함한다.

- 제안 상세에서 의미 차이·파일 차이·반영 조건·시도 실행·토론이 보인다. 프리셋을 고친 제안은 그 프리셋을 쓰는 모든 프로젝트를 「영향받는 프로젝트」 로 보인다.
- 화면을 연 뒤 Claude 가 같은 브랜치를 고치면, 반영 버튼이 막히고 서버도 반영을 거절한다.
- 팀원이 반영을 시도하면 403 과 이유. 관리자가 시도 실행 없이 반영하려면 사유를 요구한다.
- `run` 을 바꾼 제안에 [실행], `min_tests` 를 낮춘 제안에 [동작·완화] 가 붙는다.
- 레시피 파일·댓글에 `<script>` 를 넣어도 글자 그대로 보인다(CSP 위반 보고 0건).
- 리뷰어가 단 댓글을 Claude 가 `recipe_proposal` 로 읽고 고친 뒤, 반영 조건이 새 head 로 다시 계산된다.
- 일회용 로그인 링크는 두 번째 사용과 5분 뒤 사용이 실패한다.

---

## 15. 확인 필요 · 1단계 밖

| 항목 | 메모 |
|---|---|
| 새 제안·반영 알림(메신저) | 1단계 밖. 필요해지면 webhook 하나 |
| 줄 단위 댓글을 의미 차이 화면에도 달지 | 1단계는 파일 차이에만 |
| 화면에서 시도 실행 시작 버튼 | 1단계 밖 — 지금은 MCP 로. 리뷰어가 자주 원하면 `test` 와 같은 검증을 거쳐 추가 |
| 세션 수명·로그인 빈도 제한 값 | 운영해 보고 조정 |
