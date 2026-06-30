//! `scv` 바이너리 — 합성 루트(composition root).
//!
//! 여기서만 구체 타입을 안다. 설정을 읽고, 프로바이더/도구/스킬을 만들어 `Agent` 에
//! 주입하고, 인터랙티브 TUI(또는 원샷)를 띄운다. 비즈니스 로직은 모두 라이브러리
//! 크레이트에 있고 main 은 "조립 + 부트스트랩"만 담당한다.

mod project_context;
mod session_store;
mod workspace;

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

    /// 도구 스키마를 보내지 않는다(tool calling 을 지원하지 않는 모델·게이트웨이용).
    /// 텍스트 스트리밍·하네스만 확인할 때 쓴다.
    #[arg(long)]
    no_tools: bool,

    /// 세션 격리: cwd 가 git repo 면 세션별 worktree 를 만들어 그 안에서 작업한다
    /// (동시 세션이 같은 파일을 건드리는 충돌 방지, 종료 시 정리). ARCHITECTURE §4.2.
    #[arg(long)]
    isolate: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();

    // 0. 작업 디렉터리 확인 + scv 자기 소스 레포 안에서는 실행 거부(자기 코드를 작업 대상으로
    //    삼는 사고 방지). 외부 프로젝트에서만 쓰도록 한다. `SCV_ALLOW_IN_REPO=1` 로 강제 해제.
    let cwd = std::env::current_dir().context("현재 디렉터리 확인 실패")?;
    if std::env::var_os("SCV_ALLOW_IN_REPO").is_none() {
        if let Some(root) = scv_repo_root() {
            if is_within(&cwd, &root) {
                anyhow::bail!(
                    "scv 는 자기 소스 레포({}) 안에서는 실행하지 않는다 — 작업할 다른 \
                     프로젝트 디렉터리에서 실행하라. (개발 중 강제로 돌리려면 SCV_ALLOW_IN_REPO=1)",
                    root.display()
                );
            }
        }
    }

    // 0.5. 첫 실행이면 cwd 에 프로젝트 마커 `./.scv/` 를 만든다(claude 의 `.claude/`,
    //   codex 의 `.codex/` 처럼). 전역 설정·세션·worktree 는 여전히 `~/.scv/` 아래 있고,
    //   이 디렉터리는 "여기서 scv 를 썼다"는 표식이자 프로젝트 로컬 오버라이드
    //   (`./.scv/config.toml`·`./.scv/skills/`)를 둘 자리다.
    ensure_project_dir(&cwd).await;

    // 1. 설정 로드.
    let config = scv_config::Config::load().context("설정 로드 실패")?;
    let provider_id = cli.provider.as_deref().unwrap_or(&config.default_provider);
    // 설정 파일에 없으면 내장 프로바이더(ollama·aiproxy)로 폴백 → 토큰만 있으면 동작.
    let pconf = config.resolve_provider(provider_id).with_context(|| {
        format!(
            "프로바이더 `{provider_id}` 설정 없음(내장: {:?})",
            scv_config::BUILTIN_PROVIDER_IDS
        )
    })?;

    // 2. 비밀(API 키)은 환경변수에서만 읽는다. `api_key_env` 가 없으면 무인증
    //    (로컬 Ollama 등 키가 필요 없는 백엔드 — ROADMAP 4e). 키 없이 바로 동작한다.
    let api_key = match pconf.api_key_env.as_deref() {
        Some(env) => std::env::var(env).with_context(|| format!("환경변수 `{env}` 미설정"))?,
        None => String::new(),
    };

    // 3. 프로바이더 생성. auth_style 은 anthropic kind 에서만 의미(bearer=aiproxy 경유).
    let model = cli.model.clone().unwrap_or_else(|| pconf.model.clone());
    let provider = scv_providers::build(
        &pconf.kind,
        model.clone(),
        api_key,
        pconf.base_url.clone(),
        pconf.auth_style.as_deref(),
        pconf.web_search,
    )?;

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
    // 스킬 디렉터리도 선행 `~/` 를 확장한다(전역 `~/.scv/skills`). 프로젝트 로컬
    // `./.scv/skills` 는 cwd 기준 상대경로라 그대로 동작한다. 내장 compact 스킬은 항상 포함.
    let skill_dirs: Vec<std::path::PathBuf> =
        config.skills.dirs.iter().map(|d| expand_tilde(d)).collect();
    let skills = scv_skills::load_dirs(&skill_dirs).unwrap_or_default();
    // 설정 기반 정적 권한 정책. TUI 모드에서는 App 이 이 게이트를 대화형 프롬프트와 합성한다
    // (Ask → 모달). 원샷 모드에서는 비대화형이라 Ask 도구는 거부된다(명시 allow 만 실행).
    let permissions = Arc::new(build_permission_gate(&config.permissions));

    // 5. 시스템 프롬프트 합성(안정적 → 휘발성 순). cwd 는 0단계에서 구했다.
    let mut prompt = SystemPromptBuilder::new(BASE_IDENTITY)
        .environment(format!("OS: {}", std::env::consts::OS))
        .environment(format!("cwd: {}", cwd.display()));
    // 진입 컨텍스트: AGENTS.md 탐색 체인(ARCHITECTURE §4.1).
    if let Some(ctx) = project_context::load(&cwd) {
        prompt = prompt.project_context(ctx);
    }
    let system_prompt = prompt.skills(&skills).build();

    // 6. 세션 저장소 + 세션 로드(--resume)/생성. JSONL 트랜스크립트로 재개·감사 가능.
    let store = FileSessionStore::new(expand_tilde(&config.session.dir));
    let mut session = match &cli.resume {
        Some(id) => store
            .load(&SessionId(id.clone()))
            .await
            .with_context(|| format!("세션 `{id}` 로드 실패"))?,
        None => Session::new(),
    };

    // 6.5. 세션 격리(ARCHITECTURE §4.2): --isolate 면 세션별 git worktree 를 만들어 그 경로를
    //   도구 workdir 로 준다(종료 시 Drop 정리). 비격리/비-git 이면 cwd 를 그대로 쓴다.
    //   `_workspace` 는 main 끝까지 살아 있어야 worktree 가 유지된다(Drop 이 정리).
    let _workspace = workspace::SessionWorkspace::create(&cwd, &session.id.0, cli.isolate);
    let workdir = _workspace.path().to_path_buf();

    // 7. 에이전트 조립. 취소 토큰은 한 턴 동안 공유한다(원샷 Ctrl-C 도 같은 토큰을 끈다).
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
            workdir,
            cancel: cancel.clone(),
        },
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
            // `/provider`·`/model` 슬래시 명령으로 실행 중 전환할 수 있게, 프로바이더 빌드
            // 팩토리(설정·키 접근은 합성 루트만 가능)와 사용 가능한 프로바이더 목록을 주입한다.
            // 내장 프로바이더(aiproxy·ollama)도 전환 목록에 노출 → config 에 없어도 /provider 로 선택 가능.
            let provider_ids: Vec<String> = config.known_provider_ids();
            let make_provider =
                |id: &str| -> scv_core::Result<(Arc<dyn scv_core::provider::Provider>, String)> {
                    let pconf = config.resolve_provider(id).ok_or_else(|| {
                        scv_core::Error::Provider(format!("프로바이더 `{id}` 설정 없음"))
                    })?;
                    let api_key = match pconf.api_key_env.as_deref() {
                        Some(env) => std::env::var(env).map_err(|_| {
                            scv_core::Error::Provider(format!("환경변수 `{env}` 미설정"))
                        })?,
                        None => String::new(),
                    };
                    let provider = scv_providers::build(
                        &pconf.kind,
                        pconf.model.clone(),
                        api_key,
                        pconf.base_url.clone(),
                        pconf.auth_style.as_deref(),
                        pconf.web_search,
                    )?;
                    Ok((provider, pconf.model.clone()))
                };

            let spinner = scv_tui::SpinnerStyle::from_config(&config.ui.spinner);
            let mut app = scv_tui::App::new(spinner);
            app.run(
                agent,
                session,
                &store,
                &provider_ids,
                &make_provider,
                &skills,
            )
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

/// 이 바이너리를 빌드한 scv 소스 레포의 루트(빌드 시점 경로). `crates/scv-cli` 의 두 단계
/// 위가 repo 루트다. 레포가 그 자리에 없으면(이동/삭제) None.
fn scv_repo_root() -> Option<std::path::PathBuf> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")); // .../scv/crates/scv-cli
    manifest.parent()?.parent().map(|p| p.to_path_buf())
}

/// `cwd` 가 `root` 와 같거나 그 하위인가(심볼릭 링크 정규화 후 비교). 어느 쪽이든 정규화에
/// 실패하면(존재하지 않는 경로 등) `false`(차단하지 않음).
fn is_within(cwd: &std::path::Path, root: &std::path::Path) -> bool {
    match (cwd.canonicalize(), root.canonicalize()) {
        (Ok(c), Ok(r)) => c == r || c.starts_with(&r),
        _ => false,
    }
}

/// 첫 실행 시 cwd 에 프로젝트 마커 디렉터리 `./.scv/` 를 만든다(claude `.claude/`,
/// codex `.codex/` 처럼). 이미 있으면 아무것도 하지 않는다(idempotent). 핵심 상태
/// (설정·세션·worktree)는 여전히 `~/.scv/` 아래 있으므로, 만들기에 실패해도 실행을
/// 막지 않고 경고만 남긴다.
async fn ensure_project_dir(cwd: &std::path::Path) {
    let dir = cwd.join(".scv");
    if dir.exists() {
        return;
    }
    match tokio::fs::create_dir_all(&dir).await {
        Ok(()) => tracing::info!(path = %dir.display(), "프로젝트 디렉터리 생성"),
        Err(error) => {
            tracing::warn!(%error, path = %dir.display(), "프로젝트 디렉터리 생성 실패(무시하고 계속)")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_permission_maps_and_defaults_to_ask() {
        assert_eq!(parse_permission("allow"), PermissionLevel::Allow);
        assert_eq!(parse_permission("DENY"), PermissionLevel::Deny);
        assert_eq!(parse_permission("ask"), PermissionLevel::Ask);
        assert_eq!(parse_permission("nonsense"), PermissionLevel::Ask);
    }

    #[test]
    fn is_within_detects_descendant_and_rejects_sibling() {
        let base = std::env::temp_dir().join(format!("scv-within-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("repo");
        let sub = root.join("crates/scv-cli");
        let outside = base.join("other");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(&outside).unwrap();

        assert!(is_within(&root, &root), "root is within itself");
        assert!(is_within(&sub, &root), "descendant is within root");
        assert!(!is_within(&outside, &root), "sibling is not within root");
        // 존재하지 않는 경로는 차단하지 않는다(canonicalize 실패 → false).
        assert!(!is_within(&base.join("ghost"), &root));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn ensure_project_dir_creates_marker_and_is_idempotent() {
        let base = std::env::temp_dir().join(format!("scv-projdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let marker = base.join(".scv");

        // 첫 실행: 마커가 없으면 만든다.
        assert!(!marker.exists());
        ensure_project_dir(&base).await;
        assert!(marker.is_dir(), "첫 실행이 ./.scv/ 를 만든다");

        // 두 번째 실행: 이미 있으면 그대로(에러 없이 idempotent).
        ensure_project_dir(&base).await;
        assert!(marker.is_dir());

        let _ = std::fs::remove_dir_all(&base);
    }
}
