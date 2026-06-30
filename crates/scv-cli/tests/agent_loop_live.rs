//! 에이전트 루프 **라이브 종단(e2e) 테스트** — fake 가 아니라 **실제 로컬 모델**
//! (Ollama, 기본 `qwen3.5:9b`)로 `Agent::run_turn` 한 턴을 끝까지 구동한다. 합성 루트인
//! `scv-cli` 에서 실제 조립(provider → agent → tools)을 그대로 돈다.
//!
//! `tests/e2e_agent_loop.rs`(fake provider, 결정적 CI·커버리지 게이트)의 **라이브 보완**이다
//! (CODING_RULES §10). 외부 의존(실행 중인 Ollama)이 필요하므로 **기본 `#[ignore]` +
//! `SCV_E2E_OLLAMA` 게이트**이고, 파일명 `*_live` 로 `e2e_` 접두사를 피해 커버리지 e2e 티어를
//! 왜곡하지 않는다(ignore 라 자동 측정에서 안 돈다).
//!
//! 실행:
//! ```sh
//! ollama serve && ollama pull qwen3.5:9b   # 기본 모델(tool calling 지원)
//! SCV_E2E_OLLAMA=1 cargo test -p scv-cli --test agent_loop_live -- --ignored --nocapture
//! #   다른 모델: SCV_OLLAMA_MODEL=<model> ...
//! ```

use std::sync::Arc;

use scv_core::agent::{Agent, NullObserver};
use scv_core::context::NoopContextManager;
use scv_core::message::{ContentBlock, Role};
use scv_core::session::Session;
use scv_core::tool::{CancellationToken, PermissionLevel, ToolContext, ToolRegistry};
use scv_tools::{default_registry, StaticPermissionGate};

/// 라이브 게이트: `SCV_E2E_OLLAMA` 미설정이면 건너뛴다(true 면 스킵).
fn skip() -> bool {
    if std::env::var("SCV_E2E_OLLAMA").is_err() {
        eprintln!("skip: set SCV_E2E_OLLAMA=1 and run a local Ollama (ollama serve) to enable");
        return true;
    }
    false
}

fn model() -> String {
    std::env::var("SCV_OLLAMA_MODEL").unwrap_or_else(|_| "qwen3.5:9b".to_string())
}

/// 기본 프로바이더 경로(`build("ollama", ...)`)로 실제 Ollama 프로바이더를 만든다.
/// `kind="ollama"` 가 로컬 기본 base_url 과 compat 모드를 자동 적용한다.
fn ollama_agent(
    tools: ToolRegistry,
    permissions: Arc<dyn scv_core::tool::PermissionGate>,
    workdir: std::path::PathBuf,
) -> Agent {
    let provider = scv_providers::build("ollama", model(), "ollama".to_string(), None, None)
        .expect("build ollama provider");
    Agent {
        provider,
        tools,
        permissions,
        context: Arc::new(NoopContextManager),
        model: model(),
        system_prompt: "You are a concise coding assistant.".into(),
        max_tokens: 1024,
        effort: None,
        max_tool_iterations: 5,
        tool_ctx: ToolContext {
            workdir,
            cancel: CancellationToken::new(),
        },
    }
}

/// 어시스턴트가 흘린 텍스트를 전부 모은다.
fn assistant_text(session: &Session) -> String {
    session
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::Assistant))
        .flat_map(|m| &m.content)
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// 도구 없이(빈 레지스트리) 순수 텍스트 턴 — 실제 모델이 루프를 통해 답을 내는지.
#[tokio::test]
#[ignore = "requires a running local Ollama; run with SCV_E2E_OLLAMA=1 -- --ignored"]
async fn text_turn_against_real_model() {
    if skip() {
        return;
    }
    let agent = ollama_agent(
        ToolRegistry::new(), // 도구 없음 → 순수 텍스트 경로
        Arc::new(StaticPermissionGate::new(PermissionLevel::Deny)),
        std::env::temp_dir(), // 도구 미사용이라 무관
    );
    let mut session = Session::new();
    agent
        .run_turn(
            &mut session,
            "Reply with exactly one short sentence: say hello.".into(),
            &NullObserver,
        )
        .await
        .expect("turn ok — is `ollama serve` running and the model pulled?");

    // 비결정적 출력이라 **형태만** 본다: user 로 시작, assistant 로 끝, 텍스트 비지 않음.
    assert!(matches!(
        session.messages.first().map(|m| &m.role),
        Some(Role::User)
    ));
    assert!(matches!(
        session.messages.last().map(|m| &m.role),
        Some(Role::Assistant)
    ));
    let text = assistant_text(&session);
    assert!(
        !text.trim().is_empty(),
        "expected assistant text; session = {session:?}"
    );
    eprintln!("[live e2e] assistant said: {text:?}");
}

/// 실제 도구를 붙인 턴. **승인 전제**(ARCHITECTURE §4.3): `read`/`glob`/`grep`(읽기전용)은
/// 자동 허용되어 실행되지만, 모델이 `Ask` 도구(`bash`/`write`/`edit`)를 고르면 대화형 승인
/// 모달이 없어 턴이 `PermissionDenied` 로 거부된다. 모델의 도구 선택은 비결정적이라 라이브
/// 에선 **두 결과(완주 / 승인거부) 모두 정상**으로 본다 — 둘 다 "루프 + 권한 게이트가 실제
/// 모델과 배선됨"을 증명한다.
#[tokio::test]
#[ignore = "requires a running local Ollama; run with SCV_E2E_OLLAMA=1 -- --ignored"]
async fn tool_turn_against_real_model() {
    if skip() {
        return;
    }
    // read/glob/grep 만 Allow, 나머지(edit/write/bash)는 기본 Deny(fail-closed).
    let gate = StaticPermissionGate::new(PermissionLevel::Deny)
        .with_override("read", PermissionLevel::Allow)
        .with_override("glob", PermissionLevel::Allow)
        .with_override("grep", PermissionLevel::Allow);
    // workdir = 이 크레이트 디렉터리(실제 파일이 있어 읽기전용 도구가 의미 있게 동작).
    let workdir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let agent = ollama_agent(default_registry(), Arc::new(gate), workdir);

    let mut session = Session::new();
    // 읽기전용 도구로 유도(shell 금지). 그래도 모델이 Ask 도구를 고르면 승인 부재로 거부된다.
    let outcome = agent
        .run_turn(
            &mut session,
            "Use only the read-only file tools (glob, grep, read). Do NOT run shell commands. \
             List the files here and summarize what this crate is."
                .into(),
            &NullObserver,
        )
        .await;

    match outcome {
        Ok(()) => {
            assert!(matches!(
                session.messages.last().map(|m| &m.role),
                Some(Role::Assistant)
            ));
            let used_tool = session
                .messages
                .iter()
                .flat_map(|m| &m.content)
                .any(|b| matches!(b, ContentBlock::ToolUse { .. }));
            let text = assistant_text(&session);
            eprintln!("[live e2e] completed; tool used: {used_tool}; assistant: {text:?}");
            assert!(
                !text.trim().is_empty(),
                "expected assistant text; session = {session:?}"
            );
        }
        // 승인 전제: Ask 도구를 고르면 모달(2a) 전까지는 거부된다 — 라이브에서 정상 결과.
        Err(scv_core::Error::PermissionDenied(tool)) => {
            eprintln!("[live e2e] tool `{tool}` denied (no approval modal yet — expected pre-2a)");
        }
        Err(e) => panic!("unexpected error (is `ollama serve` running?): {e:?}"),
    }
}
