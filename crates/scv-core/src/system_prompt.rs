//! 계층형 시스템 프롬프트 빌더.
//!
//! 시스템 프롬프트는 여러 출처를 합쳐 만든다. **안정적인 것(거의 안 변함)을 앞에,
//! 휘발성 높은 것(매 요청 변함)을 뒤에** 배치한다 — 프롬프트 캐시는 prefix-match 라서
//! 앞부분이 고정돼야 캐시 히트가 난다.
//!
//! 합성 순서:
//!   1. base identity      — 에이전트 정체성/행동 규칙 (정적)
//!   2. environment        — OS, cwd, 날짜 등 (세션 단위)
//!   3. project context    — AGENTS.md / 프로젝트 규약 (세션 단위)
//!   4. available skills    — 스킬 name+description 목록 (세션 단위)
//!   5. dynamic reminders   — 런타임 주입 메모 (턴 단위, 가장 뒤)

use crate::skill::SkillRegistry;

/// 시스템 프롬프트 조각을 모아 최종 문자열을 만든다.
#[derive(Debug, Default)]
pub struct SystemPromptBuilder {
    base: String,
    environment: Vec<String>,
    project: Option<String>,
    skills: Vec<String>,
    reminders: Vec<String>,
}

impl SystemPromptBuilder {
    /// 에이전트 기본 정체성/규칙으로 시작한다.
    pub fn new(base_identity: impl Into<String>) -> Self {
        Self {
            base: base_identity.into(),
            ..Default::default()
        }
    }

    /// 환경 정보 한 줄(예: "OS: macOS", "cwd: /repo").
    pub fn environment(mut self, line: impl Into<String>) -> Self {
        self.environment.push(line.into());
        self
    }

    /// 프로젝트 규약(AGENTS.md 등) 본문.
    pub fn project_context(mut self, body: impl Into<String>) -> Self {
        self.project = Some(body.into());
        self
    }

    /// 레지스트리의 스킬 요약을 "사용 가능한 스킬" 섹션으로 추가한다.
    pub fn skills(mut self, registry: &SkillRegistry) -> Self {
        for meta in registry.summaries() {
            self.skills
                .push(format!("- {}: {}", meta.name, meta.description));
        }
        self
    }

    /// 휘발성 동적 리마인더(가장 뒤에 붙는다).
    pub fn reminder(mut self, line: impl Into<String>) -> Self {
        self.reminders.push(line.into());
        self
    }

    /// 최종 시스템 프롬프트 문자열을 만든다.
    pub fn build(self) -> String {
        let mut out = String::new();
        out.push_str(&self.base);

        if !self.environment.is_empty() {
            out.push_str("\n\n# Environment\n");
            out.push_str(&self.environment.join("\n"));
        }
        if let Some(project) = &self.project {
            out.push_str("\n\n# Project context\n");
            out.push_str(project);
        }
        if !self.skills.is_empty() {
            out.push_str("\n\n# Available skills\n");
            out.push_str(&self.skills.join("\n"));
        }
        if !self.reminders.is_empty() {
            out.push_str("\n\n# Reminders\n");
            out.push_str(&self.reminders.join("\n"));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill::{Skill, SkillMeta, SkillRegistry};

    #[test]
    fn build_orders_sections_stable_to_volatile() {
        let mut skills = SkillRegistry::new();
        skills.insert(Skill {
            meta: SkillMeta {
                name: "pdf".into(),
                description: "make pdf".into(),
                when_to_use: None,
            },
            dir: std::path::PathBuf::from("/x"),
            body: None,
        });
        let out = SystemPromptBuilder::new("BASE")
            .environment("OS: test")
            .project_context("PROJECT RULES")
            .skills(&skills)
            .reminder("REMIND")
            .build();
        // 순서: base → environment → project → skills → reminders (캐시 친화 prefix).
        let base = out.find("BASE").unwrap();
        let env = out.find("# Environment").unwrap();
        let proj = out.find("# Project context").unwrap();
        let sk = out.find("# Available skills").unwrap();
        let rem = out.find("# Reminders").unwrap();
        assert!(base < env && env < proj && proj < sk && sk < rem);
        assert!(out.contains("PROJECT RULES"));
        assert!(out.contains("- pdf: make pdf"));
        assert!(out.contains("REMIND"));
    }

    #[test]
    fn build_omits_empty_sections() {
        assert_eq!(SystemPromptBuilder::new("BASE").build(), "BASE");
    }
}
