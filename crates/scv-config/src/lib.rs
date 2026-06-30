//! 설정 로딩 — **다단계 병합**(뒤가 앞을 덮음, `docs/ROADMAP.md` 4d):
//!   내장 기본값(serde default) → `~/.scv/config.toml`(또는 `SCV_CONFIG`)
//!   → `./.scv/config.toml`(프로젝트, cwd 기준) → 환경변수 `SCV_*`.
//! CLI 플래그는 그 위에서 합성 루트(scv-cli)가 덮는다(`--provider`/`--model`/… ).
//!
//! 병합은 [`figment`] 으로 한다. 환경변수 중첩 키는 `__` 로 구분한다
//! (예: `SCV_AGENT__MAX_TOKENS=32000` → `agent.max_tokens`, `SCV_DEFAULT_PROVIDER=ollama`).
//!
//! 비밀(API 키)은 설정 파일에 두지 않는다. 설정에는 "키를 읽어올 환경변수 이름"
//! (`api_key_env`)만 두고, 실제 값은 런타임에 환경에서 읽는다.

#![warn(rust_2018_idioms, unreachable_pub)]

use std::path::{Path, PathBuf};

use figment::providers::{Env, Format, Toml};
use figment::Figment;
use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    // figment::Error 가 커서(>200B) Result 가 비대해진다 → 박싱(clippy::result_large_err).
    #[error("config merge/parse failed: {0}")]
    Figment(Box<figment::Error>),
}

/// 최상위 설정.
#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    pub default_provider: String,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub session: SessionConfig,
    #[serde(default)]
    pub skills: SkillsConfig,
    #[serde(default)]
    pub permissions: PermissionsConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AgentConfig {
    pub max_tokens: u32,
    pub effort: String,
    pub max_tool_iterations: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_tokens: 16000,
            effort: "high".into(),
            max_tool_iterations: 50,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionConfig {
    pub dir: String,
    pub compact_threshold_tokens: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            dir: "~/.scv/sessions".into(),
            compact_threshold_tokens: 150_000,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SkillsConfig {
    #[serde(default)]
    pub dirs: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct PermissionsConfig {
    #[serde(default)]
    pub default: Option<String>,
    #[serde(default)]
    pub tools: std::collections::BTreeMap<String, String>,
}

/// UI / 진행 표시(TUI) 설정.
#[derive(Debug, Clone, Deserialize)]
pub struct UiConfig {
    /// 스피너 글리프 선택: `auto`(터미널 유니코드 감지) | `unicode`(Braille) | `ascii`(`|/-\`).
    /// 색 출력은 `NO_COLOR` 환경변수를 존중한다(별도 키 없음). 해석은 `scv-tui`.
    pub spinner: String,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            spinner: "auto".into(),
        }
    }
}

/// 프로바이더 정의. `kind` 로 어떤 어댑터를 만들지 결정한다.
#[derive(Debug, Clone, Deserialize)]
pub struct ProviderConfig {
    pub id: String,
    pub kind: String, // "anthropic" | "openai" | "openai-compat" | "ollama"
    pub model: String,
    /// 키를 읽어올 환경변수 이름. **생략하면 무인증**(로컬 Ollama 등 키가 필요 없는
    /// 백엔드) — 이때 어댑터는 Authorization 헤더를 보내지 않는다(ROADMAP 4e).
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub anthropic_version: Option<String>,
    /// 인증 헤더 방식(anthropic kind 전용). 생략/`"x-api-key"` = Anthropic 직결(`x-api-key`),
    /// `"bearer"` = `Authorization: Bearer`(aiproxy 등 게이트웨이 경유). 다른 kind 는 무시.
    #[serde(default)]
    pub auth_style: Option<String>,
}

impl Config {
    /// 다단계 병합으로 설정을 읽는다(뒤가 앞을 덮음): 사용자 파일 → 프로젝트 파일 → `SCV_*`
    /// 환경변수. 누락 파일은 건너뛴다(빈 레이어). 서브섹션의 빠진 값은 serde 기본값으로 채워진다.
    pub fn load() -> Result<Self, ConfigError> {
        Self::figment(&Self::user_path(), &Self::project_path())
            .extract()
            .map_err(|e| ConfigError::Figment(Box::new(e)))
    }

    /// 병합 레이어를 조립한다(테스트가 경로를 주입할 수 있게 분리). `Env` 레이어는 프로세스
    /// 환경을 읽으므로 항상 마지막(최우선)이다.
    fn figment(user: &Path, project: &Path) -> Figment {
        Figment::new()
            .merge(Toml::file(user))
            .merge(Toml::file(project))
            .merge(Env::prefixed("SCV_").split("__"))
    }

    /// 사용자 설정 경로. `SCV_CONFIG` 가 있으면 그 경로, 없으면 `~/.scv/config.toml`.
    /// **cwd 와 무관**(홈 기준). 설정·스킬·세션·worktree 가 모두 `~/.scv/` 아래 모인다
    /// (Claude `~/.claude`, Codex `~/.codex` 처럼 단일 홈).
    fn user_path() -> PathBuf {
        if let Some(custom) = std::env::var_os("SCV_CONFIG") {
            return PathBuf::from(custom);
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        home.join(".scv/config.toml")
    }

    /// 프로젝트 설정 경로(cwd 기준 `./.scv/config.toml`). 작업 중인 repo 별 오버라이드.
    fn project_path() -> PathBuf {
        PathBuf::from("./.scv/config.toml")
    }

    /// 기본 프로바이더 설정을 찾는다(설정 파일에 있는 것만; 내장 폴백은 [`Self::resolve_provider`]).
    pub fn default_provider(&self) -> Option<&ProviderConfig> {
        self.providers
            .iter()
            .find(|p| p.id == self.default_provider)
    }

    /// id 로 프로바이더를 해석한다. 설정 파일의 `[[providers]]` 를 우선 찾고, 없으면 잘 알려진
    /// **내장 프로바이더**([`builtin_provider`])로 폴백한다 — 덕분에 `~/.scv/config.toml` 에
    /// 항목이 없어도 `--provider aiproxy`(또는 `ollama`)가 토큰만으로 동작한다(out-of-box).
    /// 설정 파일에서 같은 id 를 정의하면 그쪽이 내장값을 덮는다.
    pub fn resolve_provider(&self, id: &str) -> Option<ProviderConfig> {
        self.providers
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .or_else(|| builtin_provider(id))
    }

    /// 설정에 정의된 id + 내장 id 의 합집합(중복 제거, 설정 순서 우선). TUI `/provider` 전환 목록용.
    pub fn known_provider_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.providers.iter().map(|p| p.id.clone()).collect();
        for &b in BUILTIN_PROVIDER_IDS {
            if !ids.iter().any(|id| id == b) {
                ids.push(b.to_string());
            }
        }
        ids
    }
}

/// 설정 없이도 토큰/데몬만으로 동작하는 내장 프로바이더 id.
pub const BUILTIN_PROVIDER_IDS: &[&str] = &["ollama", "aiproxy"];

/// 잘 알려진 내장 프로바이더 정의. 비밀은 담지 않고 `api_key_env`(읽어올 환경변수 이름)만 둔다.
/// - `ollama`: 로컬 무인증(데몬만 띄우면 동작).
/// - `aiproxy`: 사내 게이트웨이 경유 Anthropic — `CODEB_TOKEN` 만 있으면 동작(Bearer 인증,
///   base_url 끝 `/anthropic`, 기본 모델 Sonnet 4.6).
fn builtin_provider(id: &str) -> Option<ProviderConfig> {
    match id {
        "ollama" => Some(ProviderConfig {
            id: "ollama".into(),
            kind: "ollama".into(),
            model: "qwen3.5:9b".into(),
            api_key_env: None,
            base_url: None,
            anthropic_version: None,
            auth_style: None,
        }),
        "aiproxy" => Some(ProviderConfig {
            id: "aiproxy".into(),
            kind: "anthropic".into(),
            model: "claude-sonnet-4-6".into(),
            api_key_env: Some("CODEB_TOKEN".into()),
            base_url: Some("https://aiproxy-api.backoffice.bagelgames.com/anthropic".into()),
            anthropic_version: Some("2023-06-01".into()),
            auth_style: Some("bearer".into()),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_config_with_defaults() {
        let toml_str = r#"
default_provider = "openai"

[[providers]]
id = "openai"
kind = "openai"
model = "gpt-5.5"
api_key_env = "OPENAI_API_KEY"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.default_provider, "openai");
        assert_eq!(cfg.providers.len(), 1);
        assert_eq!(cfg.default_provider().expect("provider").model, "gpt-5.5");
        // 생략된 섹션은 기본값으로 채워진다(serde default).
        assert_eq!(cfg.agent.max_tokens, 16000);
        assert_eq!(cfg.session.compact_threshold_tokens, 150_000);
        // [ui] 생략 시 spinner 기본값 "auto".
        assert_eq!(cfg.ui.spinner, "auto");
    }

    #[test]
    fn provider_parses_auth_style_for_aiproxy() {
        // aiproxy 경유 Anthropic: bearer 인증 + /anthropic 경로 + 프록시 토큰 env.
        let toml_str = r#"
default_provider = "aiproxy"
[[providers]]
id = "aiproxy"
kind = "anthropic"
model = "claude-sonnet-4-6"
api_key_env = "CODEB_TOKEN"
base_url = "https://aiproxy-api.backoffice.bagelgames.com/anthropic"
auth_style = "bearer"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("parse");
        let p = cfg.default_provider().expect("provider");
        assert_eq!(p.auth_style.as_deref(), Some("bearer"));
        assert_eq!(p.kind, "anthropic");
    }

    #[test]
    fn provider_auth_style_defaults_to_none() {
        // 생략하면 None → 어댑터가 기본 x-api-key(직결)로 동작.
        let toml_str = r#"
default_provider = "anthropic"
[[providers]]
id = "anthropic"
kind = "anthropic"
model = "claude-opus-4-8"
api_key_env = "ANTHROPIC_API_KEY"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("parse");
        assert!(cfg
            .default_provider()
            .expect("provider")
            .auth_style
            .is_none());
    }

    #[test]
    fn provider_without_api_key_env_is_keyless() {
        // api_key_env 생략 → None(무인증, 로컬 Ollama 등).
        let toml_str = r#"
default_provider = "ollama"
[[providers]]
id = "ollama"
kind = "ollama"
model = "qwen3.5:9b"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("parse");
        let p = cfg.default_provider().expect("provider");
        assert!(p.api_key_env.is_none());
    }

    #[test]
    fn parses_ui_spinner_override() {
        let toml_str = r#"
default_provider = "ollama"
[ui]
spinner = "ascii"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.ui.spinner, "ascii");
    }

    #[test]
    fn unknown_default_provider_resolves_to_none() {
        let cfg: Config = toml::from_str("default_provider = \"missing\"\n").expect("parse");
        assert!(cfg.default_provider().is_none());
    }

    #[test]
    fn resolve_provider_falls_back_to_builtin_aiproxy() {
        // config 에 aiproxy 항목이 없어도 내장 폴백으로 해석된다 → 토큰만 있으면 동작.
        let cfg: Config = toml::from_str("default_provider = \"ollama\"\n").expect("parse");
        let p = cfg.resolve_provider("aiproxy").expect("builtin aiproxy");
        assert_eq!(p.kind, "anthropic");
        assert_eq!(p.auth_style.as_deref(), Some("bearer"));
        assert_eq!(p.api_key_env.as_deref(), Some("CODEB_TOKEN"));
        assert!(p.base_url.as_deref().unwrap().ends_with("/anthropic"));
        assert_eq!(p.model, "claude-sonnet-4-6");
        // ollama 도 내장(무인증).
        assert!(cfg
            .resolve_provider("ollama")
            .unwrap()
            .api_key_env
            .is_none());
        // 미지의 id 는 None.
        assert!(cfg.resolve_provider("nope").is_none());
    }

    #[test]
    fn resolve_provider_config_overrides_builtin() {
        // 같은 id 를 config 에서 정의하면 내장값을 덮는다.
        let toml_str = r#"
default_provider = "aiproxy"
[[providers]]
id = "aiproxy"
kind = "anthropic"
model = "claude-haiku-4-5"
api_key_env = "AI_PROXY_PERSONAL_TOKEN"
base_url = "https://custom.example.com/anthropic"
auth_style = "bearer"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("parse");
        let p = cfg.resolve_provider("aiproxy").expect("configured aiproxy");
        assert_eq!(p.model, "claude-haiku-4-5"); // config 값
        assert_eq!(p.api_key_env.as_deref(), Some("AI_PROXY_PERSONAL_TOKEN"));
    }

    #[test]
    fn known_provider_ids_unions_builtins_without_dupes() {
        let toml_str = r#"
default_provider = "ollama"
[[providers]]
id = "ollama"
kind = "ollama"
model = "qwen3.5:9b"
"#;
        let cfg: Config = toml::from_str(toml_str).expect("parse");
        let ids = cfg.known_provider_ids();
        // ollama 는 config 에 있으니 한 번만, aiproxy 는 내장이라 추가됨.
        assert_eq!(ids.iter().filter(|i| *i == "ollama").count(), 1);
        assert!(ids.contains(&"aiproxy".to_string()));
    }

    #[test]
    fn user_path_respects_scv_config_env() {
        std::env::set_var("SCV_CONFIG", "/tmp/custom-scv.toml");
        assert_eq!(Config::user_path(), PathBuf::from("/tmp/custom-scv.toml"));
        std::env::remove_var("SCV_CONFIG");
    }

    /// 프로젝트 파일이 사용자 파일을 키 단위로 덮어쓴다(다단계 병합, 4d).
    #[test]
    fn project_layer_overrides_user_layer() {
        let dir = std::env::temp_dir().join(format!("scv-cfg-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.toml");
        let project = dir.join("project.toml");
        std::fs::write(
            &user,
            r#"
default_provider = "ollama"
[agent]
max_tokens = 16000
effort = "high"
max_tool_iterations = 50
[[providers]]
id = "ollama"
kind = "ollama"
model = "qwen3.5:9b"
"#,
        )
        .unwrap();
        // 프로젝트는 default_provider 와 agent.max_tokens 만 덮는다.
        std::fs::write(
            &project,
            "default_provider = \"openai\"\n[agent]\nmax_tokens = 32000\n",
        )
        .unwrap();

        let cfg: Config = Config::figment(&user, &project).extract().expect("merge");
        assert_eq!(cfg.default_provider, "openai"); // 프로젝트가 덮음
        assert_eq!(cfg.agent.max_tokens, 32000); // 프로젝트가 덮음
                                                 // 프로젝트가 안 건드린 사용자/기본값은 유지된다.
        assert_eq!(cfg.agent.effort, "high");
        assert_eq!(cfg.providers.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 누락된 프로젝트 파일은 빈 레이어로 건너뛴다(사용자 설정만으로 동작).
    #[test]
    fn missing_project_file_is_skipped() {
        let dir = std::env::temp_dir().join(format!("scv-cfg-noproj-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user.toml");
        std::fs::write(&user, "default_provider = \"ollama\"\n").unwrap();
        let cfg: Config = Config::figment(&user, &dir.join("absent.toml"))
            .extract()
            .expect("merge");
        assert_eq!(cfg.default_provider, "ollama");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
