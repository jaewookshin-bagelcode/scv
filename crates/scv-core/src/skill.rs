//! 스킬(skill) 모델.
//!
//! 스킬 = "특정 작업을 위한 절차/지식 묶음". 디렉터리 하나가 한 스킬이다:
//!
//! ```text
//! skills/
//!   pdf-report/
//!     SKILL.md          # frontmatter(name, description, when_to_use) + 본문(절차)
//!     scripts/...        # (선택) 스킬이 쓰는 보조 스크립트/리소스
//! ```
//!
//! 핵심은 **progressive disclosure**: 평소 컨텍스트에는 스킬의 `name`+`description`
//! 만 올린다(시스템 프롬프트의 "사용 가능한 스킬" 목록). 모델이 특정 스킬을 쓰기로
//! 하면 그때 본문(`body`)을 로드해 컨텍스트에 주입한다. 토큰을 아끼면서 필요한 순간에만
//! 상세 지침을 제공한다.
//!
//! 코어는 데이터 모델과 [`SkillRegistry`] 만 정의한다. 디스크에서 SKILL.md 를 읽어
//! 들이는 로더는 `scv-skills` 크레이트가 제공한다.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// SKILL.md 프론트매터(메타데이터). 항상 컨텍스트에 올라가는 부분.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    /// 고유 이름(kebab-case 권장).
    pub name: String,
    /// 한 줄 요약. 모델이 "이 스킬을 쓸지" 판단하는 근거.
    pub description: String,
    /// 언제 발동해야 하는지에 대한 힌트(트리거 조건).
    #[serde(default)]
    pub when_to_use: Option<String>,
}

/// 로드된 스킬. `body` 는 필요할 때만 채운다(progressive disclosure).
#[derive(Debug, Clone)]
pub struct Skill {
    pub meta: SkillMeta,
    /// 스킬 디렉터리 경로(본문/리소스 로드용).
    pub dir: PathBuf,
    /// SKILL.md 본문(절차 텍스트). 발동 전에는 None 일 수 있다.
    pub body: Option<String>,
}

/// 이름 → 스킬 매핑.
#[derive(Debug, Clone, Default)]
pub struct SkillRegistry {
    skills: BTreeMap<String, Skill>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, skill: Skill) {
        self.skills.insert(skill.meta.name.clone(), skill);
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.skills.get(name)
    }

    /// 시스템 프롬프트에 넣을 "사용 가능한 스킬" 요약 목록(name + description).
    pub fn summaries(&self) -> impl Iterator<Item = &SkillMeta> {
        self.skills.values().map(|s| &s.meta)
    }

    pub fn is_empty(&self) -> bool {
        self.skills.is_empty()
    }
}
