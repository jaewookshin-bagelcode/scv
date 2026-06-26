//! 프로젝트 진입 컨텍스트 로더 — `AGENTS.md` 탐색 체인.
//!
//! scv 가 대상 프로젝트에서 시동할 때 진입 컨텍스트 문서를 찾아 시스템 프롬프트의
//! project-context 레이어(ARCHITECTURE.md §4.1)에 합성한다. **새 파일 포맷을 만들지
//! 않고 다른 에이전트 도구와 같은 파일(`AGENTS.md`)을 그대로 읽어** 호환된다.
//!
//! (합성 루트인 cli 에 있는 이유: core 는 "어디서 컨텍스트를 읽을지"를 몰라야 한다 —
//! SessionStore 가 cli 에 있는 것과 같은 이유.)

use std::path::{Path, PathBuf};

/// 진입 컨텍스트를 찾아 병합한 문자열을 돌려준다(없으면 None).
///
/// 탐색 체인(덜 구체적 → 더 구체적 순으로 이어 붙인다 — **가까운 것이 뒤에 와 우선**):
///   사용자 전역 `~/.scv/AGENTS.md`
///     → repo 루트 `AGENTS.md`(`.git` 경계로 탐지)
///     → 루트~cwd 사이 하위 디렉터리들의 `AGENTS.md`
///
/// 각 위치에서 `AGENTS.md` 가 없으면 같은 위치의 `CLAUDE.md` 로 폴백한다. 신규 이름
/// (WORKER.md 등)은 도입하지 않는다(생태계 파편화 방지).
pub fn load(cwd: &Path) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();
    let mut seen: Vec<PathBuf> = Vec::new();

    // 1. 사용자 전역(가장 덜 구체적 — 맨 앞).
    if let Some(global) = global_context_file() {
        push_section(&global, &mut sections, &mut seen);
    }
    // 2. repo 루트 → cwd 로 내려가며 점점 더 구체적인 문서를 뒤에 덧붙인다.
    let root = repo_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    for dir in dirs_from_root_to_cwd(&root, cwd) {
        if let Some(file) = context_file_in(&dir) {
            push_section(&file, &mut sections, &mut seen);
        }
    }

    if sections.is_empty() {
        None
    } else {
        Some(sections.join("\n\n"))
    }
}

/// `.git` 경계까지 위로 올라가며 repo 루트를 찾는다(가장 가까운 것). 없으면 None.
fn repo_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|p| p.join(".git").exists())
        .map(Path::to_path_buf)
}

/// repo 루트부터 cwd 까지의 디렉터리 목록(루트가 먼저, cwd 가 마지막).
fn dirs_from_root_to_cwd(root: &Path, cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for dir in cwd.ancestors() {
        dirs.push(dir.to_path_buf());
        if dir == root {
            break;
        }
    }
    dirs.reverse();
    dirs
}

/// 한 디렉터리의 진입 컨텍스트 파일: `AGENTS.md` 우선, 없으면 `CLAUDE.md` 폴백.
fn context_file_in(dir: &Path) -> Option<PathBuf> {
    let agents = dir.join("AGENTS.md");
    if agents.is_file() {
        return Some(agents);
    }
    let claude = dir.join("CLAUDE.md");
    claude.is_file().then_some(claude)
}

/// 사용자 전역 컨텍스트 파일(`~/.scv/AGENTS.md`, 폴백 `CLAUDE.md`).
fn global_context_file() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    context_file_in(&Path::new(&home).join(".scv"))
}

/// 파일을 읽어 비어 있지 않으면 섹션으로 추가한다(canonical 경로로 중복 방지).
fn push_section(file: &Path, sections: &mut Vec<String>, seen: &mut Vec<PathBuf>) {
    let canon = file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
    if seen.contains(&canon) {
        return;
    }
    seen.push(canon);
    match std::fs::read_to_string(file) {
        Ok(text) if !text.trim().is_empty() => sections.push(text.trim_end().to_string()),
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(path = %file.display(), %error, "failed to read project context file");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 프로세스/태그로 유일한 임시 디렉터리를 만든다(테스트 간·재실행 간 충돌 방지).
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scv-pctx-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn merges_root_and_subdir_with_closest_last() {
        let root = temp_dir("merge");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "ROOT RULES").unwrap();
        let sub = root.join("pkg");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("AGENTS.md"), "SUB RULES").unwrap();

        let merged = load(&sub).expect("context found");
        assert!(merged.contains("ROOT RULES"), "merged = {merged:?}");
        assert!(merged.contains("SUB RULES"), "merged = {merged:?}");
        // 더 구체적인(가까운) SUB 가 ROOT 뒤에 온다.
        assert!(merged.find("ROOT RULES").unwrap() < merged.find("SUB RULES").unwrap());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn falls_back_to_claude_md_when_no_agents_md() {
        let root = temp_dir("fallback");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("CLAUDE.md"), "CLAUDE FALLBACK").unwrap();

        let merged = load(&root).expect("context found");
        assert!(merged.contains("CLAUDE FALLBACK"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prefers_agents_md_over_claude_md_in_same_dir() {
        let root = temp_dir("prefer");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "AGENTS WINS").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "CLAUDE LOSES").unwrap();

        let merged = load(&root).expect("context found");
        assert!(merged.contains("AGENTS WINS"));
        assert!(!merged.contains("CLAUDE LOSES"));

        let _ = std::fs::remove_dir_all(&root);
    }
}
