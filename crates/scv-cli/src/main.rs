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
use scv_core::context::NoopContextManager;
use scv_core::provider::Effort;
use scv_core::session::Session;
use scv_core::system_prompt::SystemPromptBuilder;
use scv_core::tool::{ToolContext, tokio_util_placeholder::CancellationToken};
use scv_tools::StaticPermissionGate;
use scv_core::tool::PermissionLevel;

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

    // 2. 비밀(API 키)은 환경변수에서만 읽는다.
    let api_key = std::env::var(&pconf.api_key_env)
        .with_context(|| format!("환경변수 `{}` 미설정", pconf.api_key_env))?;

    // 3. 프로바이더 생성.
    let model = cli.model.clone().unwrap_or_else(|| pconf.model.clone());
    let provider =
        scv_providers::build(&pconf.kind, model.clone(), api_key, pconf.base_url.clone())?;

    // 4. 도구/스킬/권한 구성.
    let tools = scv_tools::default_registry();
    let skills = scv_skills::load_dirs(&config.skills.dirs).unwrap_or_default();
    let permissions = Arc::new(StaticPermissionGate::new(PermissionLevel::Ask));

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

    // 6. 에이전트 조립.
    let agent = Agent {
        provider,
        tools,
        permissions,
        context: Arc::new(NoopContextManager),
        model,
        system_prompt,
        max_tokens: config.agent.max_tokens,
        effort: Some(parse_effort(&config.agent.effort)),
        max_tool_iterations: config.agent.max_tool_iterations,
        tool_ctx: ToolContext { workdir: cwd, cancel: CancellationToken },
    };

    // 7. 세션 준비(이어가기 or 새 세션).
    let mut session = Session::new();

    // 8. 모드 분기.
    match cli.prompt {
        Some(prompt) => {
            // 원샷: 스트림을 stdout 으로 흘린다.
            let observer = scv_tui::StreamObserver;
            agent.run_turn(&mut session, prompt, &observer).await?;
        }
        None => {
            // 인터랙티브 TUI.
            let mut app = scv_tui::App::new();
            app.run().await?;
        }
    }

    // TODO(resume): cli.resume 가 있으면 SessionStore 에서 로드해 이어간다.
    let _ = &cli.resume;
    Ok(())
}

/// 에이전트 기본 정체성/행동 규칙. 시스템 프롬프트의 안정적 prefix.
const BASE_IDENTITY: &str = "You are scv, a coding agent that works in the user's terminal. \
Be concise. Prefer using tools to inspect the codebase over guessing.";

fn parse_effort(s: &str) -> Effort {
    match s {
        "low" => Effort::Low,
        "medium" => Effort::Medium,
        "xhigh" => Effort::XHigh,
        "max" => Effort::Max,
        _ => Effort::High,
    }
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_env("SCV_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).with_writer(std::io::stderr).init();
}
