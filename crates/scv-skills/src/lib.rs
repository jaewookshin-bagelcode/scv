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

use std::path::{Path, PathBuf};

use scv_core::skill::{Skill, SkillMeta, SkillRegistry};

/// 바이너리에 임베드되는 내장 스킬. 파일·설정·설치 방식과 무관하게 **항상 포함**된다
/// (사용자가 같은 이름의 스킬을 dir 에 두면 그게 덮어쓴다).
const COMPACT_SKILL_MD: &str = include_str!("builtin/compact.md");

/// 내장 스킬만 담은 레지스트리(현재 `compact` 하나). [`load_dirs`] 가 이걸 토대로 시작한다.
pub fn builtin_registry() -> SkillRegistry {
    let mut registry = SkillRegistry::new();
    match parse_skill(COMPACT_SKILL_MD, PathBuf::new()) {
        Ok(skill) => registry.insert(skill),
        // 임베드 스킬 파싱 실패는 빌드/소스 버그 — 경고만 남기고 빈 채로 진행.
        Err(e) => tracing::warn!(error = %e, "builtin compact skill failed to parse"),
    }
    registry
}

/// 여러 디렉터리를 훑어 스킬을 로드한다. **내장 스킬**(compact)을 토대로 시작하고, 디렉터리
/// 스킬이 같은 이름을 덮어쓴다(뒤 디렉터리가 앞을 덮음). 없는/못 읽는 디렉터리는 건너뛴다
/// (에러로 중단하지 않음) → 내장 스킬은 어떤 경우에도 보존된다.
pub fn load_dirs<P: AsRef<Path>>(dirs: &[P]) -> anyhow::Result<SkillRegistry> {
    let mut registry = builtin_registry();
    for dir in dirs {
        let dir = dir.as_ref();
        if !dir.is_dir() {
            continue;
        }
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!(path = %dir.display(), error = %e, "skills dir unreadable; skipping");
                continue;
            }
        };
        for entry in entries.flatten() {
            let skill_dir = entry.path();
            let manifest = skill_dir.join("SKILL.md");
            if manifest.is_file() {
                match load_one(&skill_dir, &manifest) {
                    Ok(skill) => registry.insert(skill),
                    Err(e) => {
                        tracing::warn!(path = %manifest.display(), error = %e, "skill load failed")
                    }
                }
            }
        }
    }
    Ok(registry)
}

/// SKILL.md 한 개를 파일에서 읽어 파싱한다.
fn load_one(dir: &Path, manifest: &Path) -> anyhow::Result<Skill> {
    let raw = std::fs::read_to_string(manifest)?;
    parse_skill(&raw, dir.to_path_buf())
}

/// SKILL.md 문자열을 파싱한다(frontmatter + 본문 분리). 파일/임베드 공용.
fn parse_skill(raw: &str, dir: PathBuf) -> anyhow::Result<Skill> {
    let (front, body) = split_frontmatter(raw)
        .ok_or_else(|| anyhow::anyhow!("SKILL.md missing `---` frontmatter"))?;
    let meta: SkillMeta = serde_yaml::from_str(front)?;
    Ok(Skill {
        meta,
        dir,
        body: Some(body.to_string()),
    })
}

/// `---\n<yaml>\n---\n<body>` 를 (yaml, body) 로 가른다.
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    let rest = raw.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let front = &rest[..end];
    let body = &rest[end + "\n---\n".len()..];
    Some((front, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_frontmatter_from_body() {
        let (front, body) = split_frontmatter("---\nname: x\n---\nbody here").expect("split");
        assert!(front.contains("name: x"));
        assert_eq!(body, "body here");
    }

    #[test]
    fn split_requires_leading_fence() {
        assert!(split_frontmatter("no fence at all").is_none());
    }

    #[test]
    fn load_dirs_reads_skill_md_into_registry() {
        let dir = std::env::temp_dir().join(format!("scv-skills-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let skill_dir = dir.join("pdf");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: pdf-report\ndescription: make a PDF\n---\nsteps...",
        )
        .unwrap();

        let reg = load_dirs(std::slice::from_ref(&dir)).expect("load");
        let skill = reg.get("pdf-report").expect("registered");
        assert_eq!(skill.meta.description, "make a PDF");
        assert_eq!(skill.body.as_deref(), Some("steps..."));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn builtin_compact_skill_always_present() {
        // 디렉터리가 하나도 없어도(빈 슬라이스) 내장 compact 스킬은 들어 있다.
        let reg = load_dirs::<&Path>(&[]).expect("load");
        let compact = reg.get("compact").expect("builtin compact present");
        assert!(compact
            .body
            .as_deref()
            .unwrap_or_default()
            .contains("목표:"));
        // builtin_registry 도 단독으로 compact 를 담는다.
        assert!(builtin_registry().get("compact").is_some());
    }

    #[test]
    fn missing_or_tilde_dir_is_skipped_not_errored() {
        // 존재하지 않는 경로(틸드 미확장 등)는 에러 없이 건너뛰고 내장 스킬은 유지된다.
        let reg = load_dirs(&["~/definitely/not/here", "/no/such/scv/skills/xyz"]).expect("ok");
        assert!(reg.get("compact").is_some());
    }

    #[test]
    fn dir_skill_overrides_builtin_of_same_name() {
        let dir = std::env::temp_dir().join(format!("scv-skills-ovr-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let skill_dir = dir.join("compact");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: compact\ndescription: custom compact\n---\ncustom body",
        )
        .unwrap();

        let reg = load_dirs(std::slice::from_ref(&dir)).expect("load");
        // 같은 이름의 dir 스킬이 내장을 덮어쓴다.
        assert_eq!(
            reg.get("compact").unwrap().meta.description,
            "custom compact"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
