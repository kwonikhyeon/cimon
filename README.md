# cimon

> **Cimon says: build.**

cimon(「시몬」)은 Claude Code 가 MCP tasks 로 부르는 원격 빌드·테스트 서비스다. 팀원이 대화 중에 「이 브랜치 빌드해서 받아 줘」 하면, Windows 빌드 머신이 레시피대로 툴체인을 갖추고 빌드·테스트를 돌린 뒤 판정과 산출물 링크를 돌려준다.

**상태: 설계 초안 — 아직 구현 전.**

이 저장소는 **어떤 프로젝트를 빌드하는지 모른다.** 프로젝트별 레시피는 별도의 레시피 저장소(`cimon.toml [recipes].repo`)에 둔다.

| 문서 | 내용 |
|---|---|
| [docs/design.md](docs/design.md) | 서비스 구조 · MCP 인터페이스 · 작업 모델 · 보안 · 운영 · 구현 순서 |
| [docs/recipe-spec.md](docs/recipe-spec.md) | 레시피 파일 형식 |
| [docs/review-ui.md](docs/review-ui.md) | 레시피 리뷰 웹 화면 |
| [examples/recipes/](examples/recipes/) | 형식 예시(레시피 검사기의 시험 입력) |
