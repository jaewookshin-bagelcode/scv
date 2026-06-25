# scv 코딩 규칙

> Rust 코딩 컨벤션 + 이 프로젝트 특유의 규칙(LLM 연동, 에이전트 루프). 새 코드는 이
> 문서를 따른다. CI 가 강제하는 항목은 **[CI]** 로 표시한다.

## 0. 한눈에 보기 (체크리스트)

- [ ] `cargo fmt` 통과 **[CI]**
- [ ] `cargo clippy --all-targets --all-features -- -D warnings` 무경고 **[CI]**
- [ ] 비-테스트 코드에 `unwrap()`/`expect()`/`panic!` 없음(예외는 §2 참고)
- [ ] 라이브러리 에러는 `thiserror` enum, 바이너리에서만 `anyhow`
- [ ] 데이터/동작 분리(투명한 데이터) · 단일 책임 함수 합성 · 부작용은 가장자리로(§4.1)
- [ ] `println!`/`eprintln!` 대신 `tracing`(예외: CLI 사용자 출력)
- [ ] 비밀(API 키)은 환경변수로만, 코드/설정/로그에 없음
- [ ] 공개 API 에 doc 주석, `cargo test` 통과 **[CI]**
- [ ] 의존성은 워크스페이스 `[workspace.dependencies]` 단일 관리
- [ ] 코드 변경 시 영향받는 SSOT 문서를 같은 PR 에서 갱신(§12)

## 1. 툴체인 · 포맷 · 린트

- 툴체인은 `rust-toolchain.toml` 로 고정한다(전원 동일 버전). edition **2021**.
- 포맷은 `rustfmt.toml` 기준. **수동 정렬 금지** — `cargo fmt` 가 정답이다. **[CI]**
- clippy 를 **deny-warnings** 로 돌린다: `cargo clippy --all-targets -- -D warnings`. **[CI]**
- 각 라이브러리 크레이트 `lib.rs` 최상단에 lint 를 명시한다:
  ```rust
  #![warn(rust_2018_idioms, unreachable_pub, missing_debug_implementations)]
  ```
- 의도적으로 lint 를 끌 때는 **가장 좁은 범위**에 `#[allow(...)]` + 한 줄 사유 주석.
  크레이트 전역 `#![allow]` 금지.

## 2. 에러 처리

이 프로젝트의 가장 중요한 규칙 중 하나.

- **라이브러리 크레이트**(`scv-core`, `scv-config`, ...)는 `thiserror` 로 의미 있는
  에러 enum 을 노출한다. `#[non_exhaustive]` 를 붙여 향후 변형 추가에 대비한다.
- **바이너리**(`scv-cli`)의 `main` 부근에서만 `anyhow` 로 흡수하고, `.context(...)`
  로 사용자에게 도움이 되는 맥락을 붙인다.
- **두 가지를 섞지 않는다**: 라이브러리 함수 시그니처에 `anyhow::Result` 를 노출하지
  않는다(도구 구현 내부의 임시 사용은 예외, 단 경계에서 `Error::Tool` 로 변환).
- `unwrap()` / `expect()` / `panic!` / 배열 인덱싱 패닉은 **비-테스트 코드에서
  금지**. 유일한 예외: 증명 가능한 불변식(invariant)일 때만, **바로 위에 사유 주석**.
  ```rust
  // SAFETY: schemas() 는 등록된 도구에서만 만들어지므로 name 은 항상 존재.
  let tool = registry.get(name).expect("registered above");
  ```
- 에러는 삼키지 않는다. 복구 불가면 전파, 무시할 거면 `let _ = ...` + 사유.
- `?` 연산자를 적극 사용한다. 중첩 `match` 보다 `?` + `From` 변환이 우선.

## 3. 비동기(async) / Tokio

- 런타임은 Tokio. 진입점은 `#[tokio::main]`.
- trait 에 async 메서드가 필요하면 `#[async_trait]` 를 쓴다(이 프로젝트의 핵심
  trait — `Provider`/`Tool`/`PermissionGate`/`ContextManager`/`Observer` — 이 패턴).
- **async 함수 안에서 블로킹 호출 금지**: `std::fs`, `std::thread::sleep`, 무거운
  CPU 작업 등은 `tokio::fs` / `tokio::time::sleep` / `spawn_blocking` 으로.
  (도구 구현은 파일 IO 가 많다 → `tokio::fs` 필수.)
- 취소(cancellation)는 `CancellationToken` 으로 협조적으로 처리한다. 긴 도구는
  주기적으로 `ctx.cancel.is_cancelled()` 를 확인한다.
- 공유 상태는 `Arc<T>`(불변) 또는 `Arc<Mutex<T>>`/`Arc<RwLock<T>>`(가변). 락은
  await 지점을 넘겨 들고 있지 않는다.

## 4. 타입 · 함수 · API 설계

- **의존성 역전을 지킨다**: `scv-core` 가 trait 을 정의하고, 바깥 크레이트가 구현한다.
  core 는 어떤 구체 크레이트에도 의존하지 않는다. (이게 멀티 프로바이더의 토대다.)
- 도메인 식별자는 newtype 으로 감싼다: `SessionId(String)` (원시 `String` 남발 금지).
- 함수 인자가 많아지면(>7) 파라미터 구조체로 묶는다(clippy 가 경고).
- 빌더가 자연스러운 곳엔 빌더 패턴(`SystemPromptBuilder`). 소비형 빌더는
  `self` 를 받아 `Self` 를 반환한다.
- 공개 enum 중 향후 변형이 늘 수 있는 것은 `#[non_exhaustive]`(예: `StreamEvent`,
  `Error`).
- 외부 입력을 받는 함수는 `impl Into<String>` / `AsRef<Path>` 등으로 호출부를 편하게.
- 불필요한 `clone()` 을 피한다. 빌릴 수 있으면 빌린다. 단, 가독성을 해치는
  수명(lifetime) 곡예보다는 명시적 `clone` 이 낫다(특히 trait object 경계).

### 4.1 데이터 지향 + 함수형 — 투명한 데이터 + 단일 책임 함수 합성 (DOT · FP)

**DOT(Data-Oriented Tech stack, 데이터 지향)** 와 함수형 프로그래밍에서 영감을 얻는다.
핵심은 두 가지다: (1) **데이터와 동작을 분리**해 데이터는 투명한 값으로 두고, (2)
**기초 함수는 한 가지 일만**(단일 책임) 하게 작게 만든 뒤 **상위 함수가 그 함수들을
조합(compose)** 해 의도를 표현한다.

- **데이터와 동작을 분리한다(데이터 지향의 핵심)**: 도메인은 메서드/숨은 상태가 붙은
  객체가 아니라, **투명한 평범한 데이터**(공개 필드 struct/enum, `serde` 직렬화·비교
  가능, 가능하면 불변)로 모델링한다. 동작은 그 데이터를 받아 새 데이터를 돌려주는
  **자유 함수/변환**으로 둔다.
  - 본 저장소의 예: `Message`/`ContentBlock`/`StreamEvent` 는 동작 없는 순수 데이터고,
    변환은 어댑터(와이어↔중립)·`MessageAssembler`·빌더가 담당한다. "객체에 로직을
    넣기"보다 "데이터를 함수로 흘려보내기".
- **데이터 흐름을 중심으로 사고한다**: 입력 데이터 → 일련의 변환 → 출력 데이터. 파이프
  라인의 각 단계가 하나의 작은 함수다. (핫패스라면 메모리 레이아웃·일괄 처리까지
  고려하되, 조기 최적화는 피한다.)
- **순수 함수를 기본값으로**: 기초 함수는 가능하면 `입력 → 출력`, 부작용/IO 없음.
  테스트·재사용·합성이 쉬워진다.
- **부작용은 가장자리로(functional core, imperative shell)**: IO(파일/네트워크/LLM
  호출)는 상위 오케스트레이션(에이전트 루프, 어댑터 경계, CLI)으로 밀어내고, 안쪽
  변환/판단 로직은 순수하게 유지한다.
  - 본 저장소의 예: SSE 바이트 수신(부작용)은 `Provider` 어댑터가, 이벤트→메시지
    집계(순수 변환)는 `MessageAssembler` 가. 프롬프트 합성은 순수 빌더
    (`SystemPromptBuilder`)가 조각을 받아 문자열을 만든다.
- **합성 우선**: 명령형 루프로 짜기 전에 이터레이터 콤비네이터(`map`/`filter`/
  `fold`/`?`)와 작은 함수의 조합으로 표현 가능한지 본다.
- **단방향 데이터 흐름**: 함수는 받은 값을 변환해 돌려준다. 깊은 곳에서 공유 가변
  상태를 만지지 않는다 — 가변은 가장자리에서, 안쪽은 `&self`/불변 입력.
- **과한 분해 금지(균형)**: 이 원칙은 "무조건 잘게 쪼개 함수/파일을 늘려라"가 아니다.
  재사용·테스트·가독에 도움이 될 때 쪼갠다. 한 번만 쓰고 자명한 3줄을 함수로 빼지
  않고, 가독성을 해치는 포인트프리/과한 체이닝도 지양한다. 의도가 드러나는 선이 기준.
- **Rust 메모**: 합성에는 `impl Iterator`/제네릭을 우선하고, 핫패스가 아니면
  `Box<dyn Fn>` 같은 동적 디스패치 비용은 피한다. 단일 책임을 어겨 인자가 많아지면
  파라미터 구조체로 묶는다(clippy `too-many-arguments`/`cognitive-complexity` 가 경고).

## 5. 모듈 · 네이밍

- 모듈 1개 = 책임 1개. `lib.rs` 는 모듈 선언 + 재노출(`pub use`) + 크레이트 doc 만.
- 네이밍: 타입 `UpperCamelCase`, 함수/변수/모듈 `snake_case`, 상수 `SCREAMING_SNAKE`.
- 약어도 한 단어처럼: `HttpClient`(O), `HTTPClient`(X).
- 도구·스킬·프로바이더 id 는 **kebab-case 또는 소문자 단어**로 통일(`read`,
  `anthropic`, `pdf-report`).
- import 는 `StdExternalCrate` 그룹 순서(rustfmt 가 정렬). glob import(`use foo::*`)는
  prelude/테스트 외 금지.

## 6. 직렬화(serde)

- 와이어/디스크 타입에는 `#[derive(Serialize, Deserialize)]`. 내부 표현과 와이어
  표현이 다르면 **변환 함수를 명시**(프로바이더 어댑터가 이 역할).
- enum 직렬화는 의도를 명시: 내부 태그(`#[serde(tag = "type")]`)인지 등.
- `rename_all` 로 케이스 규약을 한 곳에서 지정(`snake_case`/`lowercase`).
- 결정적 직렬화가 필요한 곳(프롬프트 캐시 입력 등)은 정렬된 컬렉션(`BTreeMap`)을
  쓴다. `HashMap` 순회 순서에 의존 금지.

## 7. 로깅 · 관측성

- 라이브러리에서 `println!`/`eprintln!` **금지** — `tracing` 매크로(`info!`,
  `warn!`, `debug!`, `error!`)를 쓴다.
- 예외: `scv-cli`/`scv-tui` 의 **사용자 대상 출력**은 stdout(print) 가 맞다.
  진단/디버그 로그는 stderr(tracing)로 분리한다.
- 구조화 필드를 활용: `warn!(path = %p.display(), error = %e, "skill load failed")`.
- 민감정보(키/토큰/사용자 데이터)를 로그에 남기지 않는다.

## 8. 보안 / 비밀

- **API 키 등 비밀은 환경변수로만** 주입한다. 설정 파일에는 "키를 읽어올 환경변수
  이름"(`api_key_env`)만 둔다. 코드/설정/로그/세션 파일에 평문 키 금지.
- `.env` 는 커밋하지 않는다(`.gitignore` 등록). `.env.example` 만 커밋.
- 도구의 모든 **경로 입력은 `workdir` 안으로 제한**한다(canonicalize 후 prefix 검사).
  `..`, 심볼릭 링크, 절대경로 탈출을 거부한다.
- `bash`/명령 실행 입력은 **신뢰 불가 모델 출력**으로 취급한다. 격리된 환경,
  타임아웃, 허용목록(allowlist) 기준으로 다룬다. blocklist 만으로 충분하다고 보지 않는다.

## 9. LLM 연동 규칙 (프로젝트 특화)

기본 프로바이더는 **OpenAI(ChatGPT 5.5, `gpt-5.5`)**, Anthropic 은 대체다. 아래
**공통** 규칙은 모든 어댑터에, **어댑터별** 항목은 해당 프로바이더에만 적용한다.
위반하면 런타임 400/오작동.

### 공통

- **스트리밍이 기본**: 긴 출력/큰 `max_tokens` 에서 HTTP 타임아웃을 피하려면
  스트리밍이 필수다. TUI 실시간 출력에도 필요하다. `Provider::stream` 만 둔 이유.
- **모델 ID 는 설정에서 주입**, 코드에 하드코딩하지 않는다(어댑터 기본값 1개만 허용).
  기본 모델 `gpt-5.5`. 날짜/임의 접미사를 붙이지 않는다.
- **`max_tokens`**: 스트리밍 시 64000 권장, 비스트리밍 16000. 분류 등 짧은 출력만
  더 낮춘다. 무작정 낮추면 출력이 중간에 잘린다.
- **tool_use 입력은 JSON 파싱으로**: 직렬화된 문자열을 정규식/부분문자열로 매칭하지
  않는다. 프로바이더마다 이스케이프가 다르다.
- **병렬 도구 결과는 하나의 user 메시지**로 모아 보낸다. 분산 금지.
- **`stop_reason`(또는 finish_reason) 을 먼저 확인**: 거부/안전 종료 시 content 가
  없거나 부분일 수 있으므로 분기 후 본문을 읽는다.
- **에러가 HTTP 200 으로 오는 경우가 있다**: 서버측 거부/도구 결과 등. 상태코드만
  믿지 말고 종료 사유/결과 블록을 분기한다.

### OpenAI 어댑터 (기본)

- 인증 `Authorization: Bearer {OPENAI_API_KEY}`, 엔드포인트 `/chat/completions`.
- SSE delta(`choices[].delta`)와 `tool_calls` 를 코어의 `StreamEvent`/`ContentBlock`
  로 매핑한다. content 는 문자열, tool_calls 는 별도 배열 구조라는 차이를 흡수한다.
- 사고/추론 깊이는 **OpenAI 자체 파라미터**(reasoning effort 등)를 쓴다. Anthropic
  의 `thinking` 파라미터를 보내지 않는다.

### Anthropic 어댑터 (대체)

- 인증 `x-api-key` + `anthropic-version: 2023-06-01`, 엔드포인트 `/v1/messages`.
- 사고/효과는 `thinking: {type: "adaptive"}` + `output_config.effort`.
  `budget_tokens`/`temperature`/`top_p` 는 보내지 않는다(최신 모델에서 400).
- 모델 id 예: `claude-opus-4-8`(날짜 접미사 금지).

## 10. 테스트

- 단위 테스트는 모듈 내 `#[cfg(test)] mod tests`. 순수 로직(프롬프트 합성, frontmatter
  파싱, 권한 결정, 와이어 변환)은 반드시 단위 테스트한다.
- 외부 의존(LLM/네트워크)은 trait 을 mock 구현해 테스트한다(`Provider`/`Tool` 가
  trait 인 이유 중 하나). 실제 API 를 때리는 테스트는 `#[ignore]` + 환경변수 게이트.
- 통합 테스트는 `tests/`. 에이전트 루프는 fake `Provider`(미리 정해둔 이벤트 스트림을
  내는)로 종단 테스트한다.
- 테스트에서는 `unwrap()`/`expect()` 허용(실패가 곧 테스트 실패).
- 새 기능/버그 수정에는 회귀 테스트를 동반한다.

## 11. 의존성 관리

- 모든 외부 의존성은 루트 `[workspace.dependencies]` 에서 단일 버전으로 관리한다.
  개별 크레이트는 `dep.workspace = true` 로만 참조한다(버전 드리프트 방지).
- 새 의존성 추가는 PR 에서 사유를 밝힌다. 표준 라이브러리/기존 의존으로 가능하면
  추가하지 않는다.
- `Cargo.lock` 은 **커밋한다**(scv 는 애플리케이션/바이너리).

## 12. 주석 · 문서 · SSOT

- **SSOT(단일 출처) 유지**: 설계·규약·결정은 항상 SSOT 문서에 남긴다. 구현으로
  동작/인터페이스/기본값/로드맵이 바뀌면 **같은 PR 에서 해당 SSOT 를 갱신**한다
  (코드가 문서와 어긋나면 문서가 진실이 되도록). 같은 사실을 여러 문서에 복제하지
  말고 한 곳(SSOT)에 두고 나머지는 링크한다. SSOT 맵은 `AGENTS.md` § 단일 출처 규칙 참조.
- 공개 항목(타입/함수/trait)에는 `///` doc 주석. **무엇을 하는지가 아니라 왜/언제
  쓰는지**를 적는다(시그니처로 자명한 내용 반복 금지).
- 크레이트/모듈 상단 `//!` 로 책임과 모듈 지도를 설명한다(이 저장소의 기존 파일 참고).
- 미완 지점은 `// TODO(주제): ...` 로 표시하고, 가능하면 추적 이슈를 연결한다.
- 주석은 주변 코드의 밀도/톤에 맞춘다. 자명한 코드에 군더더기 주석 금지.
- 한국어 주석 OK(팀 컨벤션). 단 공개 API doc 은 핵심 용어를 영어 병기.

## 13. Git · 커밋 · PR

- 기본 브랜치에 직접 커밋하지 않는다. 기능 브랜치 → PR.
- 커밋 메시지: 명령형 한 줄 요약(72자 이내) + 필요 시 본문에 "왜".
- PR 은 작게. 리뷰어가 한 번에 파악 가능한 단위로 쪼갠다.
- PR 전 로컬에서 `cargo fmt && cargo clippy -- -D warnings && cargo test` 통과 확인.
- **PR 에 코드 변경이 있으면 영향받는 SSOT 문서 갱신을 같은 PR 에 포함한다**
  (문서 미갱신 = 미완. §12 참조).
- 리뷰는 정확성 → 안전성 → 단순성 순으로 본다. trait 경계(코어 추상)를 바꾸는 PR 은
  영향 범위를 본문에 명시한다.

## 14. 안티패턴 (하지 말 것)

- core 가 구체 크레이트(`scv-providers` 등)에 의존 → **의존성 역전 위반**.
- 비-테스트 코드의 `unwrap()`/`expect()` 남발.
- 라이브러리에 `anyhow::Result` 노출, 라이브러리에서 `println!`.
- 비밀을 설정/코드/로그에 평문 저장.
- LLM `tool_use.input` 을 문자열 매칭으로 파싱.
- 도구 경로 입력을 검증 없이 `std::fs::read`(경로 탈출 위험).
- `HashMap` 순회 순서나 `Date::now()` 같은 비결정성을 프롬프트 prefix 에 섞기(캐시 파괴).
