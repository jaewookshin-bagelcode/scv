//! 스킬 로더 — 디스크의 `SKILL.md` 들을 읽어 `scv_core::skill::SkillRegistry` 를 채운다.
//!
//! SKILL.md 형식(YAML frontmatter + Markdown 본문):
//! ```text
//! ---
//! name: pdf-report
//! description: PDF 보고서를 생성/검증한다
//! when_to_use: 사용자가 "PDF 보고서"를 요청할 때
//! ---
//! (본문: 절차 설명 ...)
//! ```
//!
//! progressive disclosure: 로드 시 `body` 까지 읽어두되, 시스템 프롬프트에는 메타만
//! 노출하고 본문은 스킬이 발동될 때 주입한다(주입 로직은 에이전트/CLI 쪽).

#![warn(rust_2018_idioms, unreachable_pub)]

use std::path::Path;

use scv_core::skill::{Skill, SkillMeta, SkillRegistry};

/// 여러 디렉터리를 훑어 스킬을 로드한다. 뒤 디렉터리가 같은 이름을 덮어쓴다.
pub fn load_dirs<P: AsRef<Path>>(dirs: &[P]) -> anyhow::Result<SkillRegistry> {
    let mut registry = SkillRegistry::new();
    for dir in dirs {
        if !dir.as_ref().is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(dir)? {
            let skill_dir = entry?.path();
            let manifest = skill_dir.join("SKILL.md");
            if manifest.is_file() {
                match load_one(&skill_dir, &manifest) {
                    Ok(skill) => registry.insert(skill),
                    Err(e) => tracing::warn!(path = %manifest.display(), error = %e, "skill load failed"),
                }
            }
        }
    }
    Ok(registry)
}

/// SKILL.md 한 개를 파싱한다(frontmatter + 본문 분리).
fn load_one(dir: &Path, manifest: &Path) -> anyhow::Result<Skill> {
    let raw = std::fs::read_to_string(manifest)?;
    let (front, body) = split_frontmatter(&raw)
        .ok_or_else(|| anyhow::anyhow!("SKILL.md missing `---` frontmatter"))?;
    let meta: SkillMeta = serde_yaml::from_str(front)?;
    Ok(Skill { meta, dir: dir.to_path_buf(), body: Some(body.to_string()) })
}

/// `---\n<yaml>\n---\n<body>` 를 (yaml, body) 로 가른다.
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let front = &rest[..end];
    let body = &rest[end + "\n---\n".len()..];
    Some((front, body))
}
