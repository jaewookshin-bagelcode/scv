//! `scv` 바이너리 — 합성 루트(composition root).
//!
//! 여기서만 구체 타입을 안다. 설정을 읽고, 프로바이더/도구/스킬을 만들어 `Agent` 에
//! 주입하고, 인터랙티브 TUI(또는 원샷)를 띄운다. 비즈니스 로직은 모두 라이브러리
//! 크레이트에 있고 main 은 "조립 + 부트스트랩"만 담당한다.

mod project_context;
mod session_store;

use std::sync::Arc;

use anyhow::Context;
use clap::Parser;
use scv_core::agent::Agent;
use scv_core::context::SummarizingContextManager;
use scv_core::provider::Effort;
use scv_core::session::{Session, SessionId, SessionStore};
use scv_core::system_prompt::SystemPromptBuilder;
use scv_core::tool::PermissionLevel;
use scv_core::tool::{CancellationToken, ToolContext};
use scv_tools::StaticPermissionGate;

use session_store::FileSessionStore;

/// 코딩 에이전트 CLI.
#[derive(Debug, Parser)]
#[command(name = "scv", version, about = "멀티 프로바이더 코딩 에이전트")]
struct Cli {
    /// 원샷 모드로 실행할 프롬프트. 생략하면 인터랙티브 TUI 로 진입한다.
    prompt: Option<String>,

    /// 사용할 프로바이더 id (설정의 default_provider 를 덮어씀).
    #[arg(long)]
    provider: Option<String>,

    /// 사용할 모델 id (프로바이더 기본값을 덮어씀).
    #[arg(long)]
    model: Option<String>,

    /// 이어서 진행할 세션 id.
    #[arg(long)]
    resume: Option<String>,

    /// 추론 강도 override (none|low|medium|high|xhigh|max). 설정값을 덮어쓴다.
    /// 비-reasoning 모델(gpt-4o 등) 수동 테스트 시 `none` 으로 reasoning_effort 를 끈다.
    #[arg(long)]
    effort: Option<String>,

    /// 도구 스키마를 보내지 않는다(tool calling 미지원 로컬 모델용, 예 gemma).
    /// 텍스트 스트리밍·하네스만 확인할 때 쓴다.
    #[arg(long)]
    no_tools: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();

    // 1. 설정 로드.
    let config = scv_config::Config::load().context("설정 로드 실패")?;
    let provider_id = cli.provider.as_deref().unwrap_or(&config.default_provider);
    let pconf = config
        .providers
        .iter()
        .find(|p| p.id == provider_id)
        .with_context(|| format!("프로바이더 `{provider_id}` 설정 없음"))?;

    // 2. 비밀(API 키)은 환경변수에서만 읽는다. `api_key_env` 가 없으면 무인증
    //    (로컬 Ollama 등 키가 필요 없는 백엔드 — ROADMAP 4e). 키 없이 바로 동작한다.
    let api_key = match pconf.api_key_env.as_deref() {
        Some(env) => std::env::var(env).with_context(|| format!("환경변수 `{env}` 미설정"))?,
        None => String::new(),
    };

    // 3. 프로바이더 생성.
    let model = cli.model.clone().unwrap_or_else(|| pconf.model.clone());
    let provider =
        scv_providers::build(&pconf.kind, model.clone(), api_key, pconf.base_url.clone())?;

    // 4. 도구/스킬/권한 구성. --no-tools 면 빈 레지스트리(도구 스키마 미전송 → tool calling
    //    미지원 로컬 모델도 텍스트로 응답).
    let tools = if cli.no_tools {
        scv_core::tool::ToolRegistry::new()
    } else {
        // transcript_search 는 세션 디렉터리 경로 주입이 필요해 합성 루트에서 등록한다
        // (과거 세션 JSONL 정밀 검색 — compaction 손실 보완).
        let mut reg = scv_tools::default_registry();
        reg.register(std::sync::Arc::new(scv_tools::TranscriptSearchTool::new(
            expand_tilde(&config.session.dir),
        )));
        reg
    };
    let skills = scv_skills::load_dirs(&config.skills.dirs).unwrap_or_default();
    // 설정 기반 정적 권한 정책. TUI 모드에서는 App 이 이 게이트를 대화형 프롬프트와 합성한다
    // (Ask → 모달). 원샷 모드에서는 비대화형이라 Ask 도구는 거부된다(명시 allow 만 실행).
    let permissions = Arc::new(build_permission_gate(&config.permissions));

    // 5. 시스템 프롬프트 합성(안정적 → 휘발성 순).
    let cwd = std::env::current_dir()?;
    let mut prompt = SystemPromptBuilder::new(BASE_IDENTITY)
        .environment(format!("OS: {}", std::env::consts::OS))
        .environment(format!("cwd: {}", cwd.display()));
    // 진입 컨텍스트: AGENTS.md 탐색 체인(ARCHITECTURE §4.1).
    if let Some(ctx) = project_context::load(&cwd) {
        prompt = prompt.project_context(ctx);
    }
    let system_prompt = prompt.skills(&skills).build();

    // 6. 에이전트 조립. 취소 토큰은 한 턴 동안 공유한다(원샷 Ctrl-C 도 같은 토큰을 끈다).
    //    컨텍스트 관리: 임계(compact_threshold_tokens) 초과 시 오래된 앞부분을 같은 모델로
    //    요약(compaction)한다. 최근 KEEP_RECENT 개 메시지는 verbatim 유지(ROADMAP 3b).
    let context = Arc::new(SummarizingContextManager::new(
        provider.clone(),
        model.clone(),
        config.session.compact_threshold_tokens,
        KEEP_RECENT_MESSAGES,
    ));
    let cancel = CancellationToken::new();
    let agent = Agent {
        provider,
        tools,
        permissions,
        context,
        model,
        system_prompt,
        max_tokens: config.agent.max_tokens,
        effort: parse_effort(cli.effort.as_deref().unwrap_or(&config.agent.effort)),
        max_tool_iterations: config.agent.max_tool_iterations,
        tool_ctx: ToolContext {
            workdir: cwd,
            cancel: cancel.clone(),
        },
    };

    // 7. 세션 저장소 + 세션 로드(--resume)/생성. JSONL 트랜스크립트로 재개·감사 가능.
    let store = FileSessionStore::new(expand_tilde(&config.session.dir));
    let mut session = match &cli.resume {
        Some(id) => store
            .load(&SessionId(id.clone()))
            .await
            .with_context(|| format!("세션 `{id}` 로드 실패"))?,
        None => Session::new(),
    };

    // 8. 모드 분기.
    match cli.prompt {
        Some(prompt) => {
            // 원샷: 스트림을 stdout 으로 흘린다. Ctrl-C 는 별도 태스크에서 토큰을 끄고,
            // run_turn 은 그것을 협조적으로 관찰해 부분 결과를 보존한 뒤 Cancelled 로 끝난다
            // (select! 로 run_turn 미래를 드롭하지 않으므로 정리 로직이 실제로 실행된다).
            let observer = scv_tui::StreamObserver;
            let signal = {
                let cancel = cancel.clone();
                tokio::spawn(async move {
                    let _ = tokio::signal::ctrl_c().await;
                    cancel.cancel();
                })
            };
            let outcome = agent.run_turn(&mut session, prompt, &observer).await;
            signal.abort();
            match outcome {
                Ok(()) => {}
                // 취소는 정상적인 중단 — 부분 결과는 이미 session 에 보존돼 있다.
                Err(scv_core::Error::Cancelled) => {
                    eprintln!("\n(interrupted — partial turn saved)")
                }
                Err(e) => return Err(anyhow::Error::new(e).context("턴 실행 실패")),
            }
            store.save(&session).await.context("세션 저장 실패")?;
            // 재개할 수 있도록 세션 id 를 알린다(`scv --resume <id>`).
            println!("[session {}]", session.id.0);
        }
        None => {
            // 인터랙티브 TUI: 대화 루프·권한 모달·인터럽트·진행 표시·턴별 세션 저장(§4.5).
            // App 이 agent.permissions 를 대화형 게이트로 감싸 Ask 도구를 모달로 승인받는다.
            let spinner = scv_tui::SpinnerStyle::from_config(&config.ui.spinner);
            let mut app = scv_tui::App::new(spinner);
            app.run(agent, session, &store)
                .await
                .context("TUI 실행 실패")?;
        }
    }

    Ok(())
}

/// 설정의 `[permissions]` 를 정적 권한 게이트로 만든다. `default` + 도구별 오버라이드.
/// 알 수 없는 값은 가장 안전한 `Ask` 로 둔다(fail-closed).
fn build_permission_gate(cfg: &scv_config::PermissionsConfig) -> StaticPermissionGate {
    let default = cfg
        .default
        .as_deref()
        .map(parse_permission)
        .unwrap_or(PermissionLevel::Ask);
    let mut gate = StaticPermissionGate::new(default);
    for (tool, level) in &cfg.tools {
        gate = gate.with_override(tool.clone(), parse_permission(level));
    }
    gate
}

/// 권한 문자열(`allow`/`ask`/`deny`)을 [`PermissionLevel`] 로. 미지의 값은 `Ask`.
fn parse_permission(s: &str) -> PermissionLevel {
    match s.trim().to_ascii_lowercase().as_str() {
        "allow" => PermissionLevel::Allow,
        "deny" => PermissionLevel::Deny,
        "ask" => PermissionLevel::Ask,
        other => {
            tracing::warn!(level = %other, "unknown permission level; defaulting to ask");
            PermissionLevel::Ask
        }
    }
}

/// 설정의 경로 문자열에서 선행 `~/` 를 홈 디렉터리로 확장한다(없으면 그대로).
fn expand_tilde(path: &str) -> std::path::PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => std::path::PathBuf::from(home).join(rest),
            None => std::path::PathBuf::from(path),
        },
        None => std::path::PathBuf::from(path),
    }
}

/// compaction 시 verbatim 으로 보존할 최근 메시지 수(그 이전은 요약으로 접는다).
const KEEP_RECENT_MESSAGES: usize = 8;

/// 에이전트 기본 정체성/행동 규칙. 시스템 프롬프트의 안정적 prefix.
const BASE_IDENTITY: &str = "You are scv, a coding agent that works in the user's terminal. \
Be concise. Prefer using tools to inspect the codebase over guessing.";

/// 설정의 effort 문자열을 파싱한다. `none`/`off` 는 **추론 파라미터를 보내지 않음**을
/// 뜻한다(비-reasoning OpenAI 모델은 `reasoning_effort` 에 400 을 내므로 이때 쓴다).
fn parse_effort(s: &str) -> Option<Effort> {
    match s {
        "none" | "off" => None,
        "low" => Some(Effort::Low),
        "medium" => Some(Effort::Medium),
        "high" => Some(Effort::High),
        "xhigh" => Some(Effort::XHigh),
        "max" => Some(Effort::Max),
        other => {
            tracing::warn!(effort = %other, "unknown effort; defaulting to high");
            Some(Effort::High)
        }
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("SCV_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
