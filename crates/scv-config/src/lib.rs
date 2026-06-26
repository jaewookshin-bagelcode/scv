//! 설정 로딩.
//!
//! **현재 구현**: `~/.config/scv/config.toml` 한 곳을 읽어 파싱한다(`Config::load`).
//! 다단계 병합은 아직 없다 — 단일 파일이 전부다.
//! **계획(`docs/ROADMAP.md` 4d)**: 뒤가 앞을 덮는 다단계 병합 —
//!   내장 기본값 → `~/.config/scv/config.toml` → `./.scv/config.toml`(프로젝트)
//!   → 환경변수(SCV_*) → CLI 플래그.
//!
//! 비밀(API 키)은 설정 파일에 두지 않는다. 설정에는 "키를 읽어올 환경변수 이름"
//! (`api_key_env`)만 두고, 실제 값은 런타임에 환경에서 읽는다.

#![warn(rust_2018_idioms, unreachable_pub)]

use std::path::PathBuf;

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("env var `{0}` (api_key_env) is not set")]
    MissingApiKey(String),
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
    pub kind: String, // "anthropic" | "openai"
    pub model: String,
    pub api_key_env: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub anthropic_version: Option<String>,
}

impl Config {
    /// `~/.config/scv/config.toml` 을 읽어 파싱한다(다단계 병합은 roadmap 4d).
    pub fn load() -> Result<Self, ConfigError> {
        let path = Self::config_path();
        let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Io {
            path: path.clone(),
            source,
        })?;
        Ok(toml::from_str(&text)?)
    }

    /// 설정 파일 경로. `SCV_CONFIG` 환경변수가 있으면 그 경로, 없으면
    /// `~/.config/scv/config.toml`. **cwd 와 무관**(홈 기준)이라 scv 를 어느 디렉터리에서
    /// 실행해도 같은 설정을 읽는다 — 작업 대상은 cwd, 설정은 홈에 둔다.
    fn config_path() -> PathBuf {
        if let Some(custom) = std::env::var_os("SCV_CONFIG") {
            return PathBuf::from(custom);
        }
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        home.join(".config/scv/config.toml")
    }

    /// 기본 프로바이더 설정을 찾는다.
    pub fn default_provider(&self) -> Option<&ProviderConfig> {
        self.providers
            .iter()
            .find(|p| p.id == self.default_provider)
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
    fn config_path_respects_scv_config_env() {
        std::env::set_var("SCV_CONFIG", "/tmp/custom-scv.toml");
        assert_eq!(Config::config_path(), PathBuf::from("/tmp/custom-scv.toml"));
        std::env::remove_var("SCV_CONFIG");
    }
}
