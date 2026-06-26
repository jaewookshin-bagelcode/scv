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
    Io { path: PathBuf, source: std::io::Error },
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
        Self { max_tokens: 16000, effort: "high".into(), max_tool_iterations: 50 }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct SessionConfig {
    pub dir: String,
    pub compact_threshold_tokens: u64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self { dir: "~/.scv/sessions".into(), compact_threshold_tokens: 150_000 }
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
    /// 표준 위치들에서 설정을 읽어 병합한다.
    pub fn load() -> Result<Self, ConfigError> {
        // TODO: figment/serde 병합으로 다단계 오버라이드 구현.
        //       지금은 골격 — 사용자 설정 파일 한 곳만 읽는다.
        let path = dirs::config_dir()
            .map(|d| d.join("scv/config.toml"))
            .unwrap_or_else(|| PathBuf::from("config/config.example.toml"));
        let text = std::fs::read_to_string(&path)
            .map_err(|source| ConfigError::Io { path: path.clone(), source })?;
        Ok(toml::from_str(&text)?)
    }

    /// 기본 프로바이더 설정을 찾는다.
    pub fn default_provider(&self) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == self.default_provider)
    }
}
