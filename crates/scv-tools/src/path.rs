//! 경로 보안 — 도구 입력 경로를 **`workdir` 안으로 제한**한다(CODING_RULES §8).
//!
//! 모델이 만든 경로는 신뢰 불가 입력이다. `..`/심볼릭 링크/절대경로로 작업 루트를
//! 벗어나려는 시도를 canonicalize 후 prefix 검사로 거부한다. 모든 파일 도구
//! (`read`/`write`/`edit`/`glob`/`grep`)가 이 헬퍼를 공유한다.

use std::path::{Path, PathBuf};

/// **이미 존재하는** 경로를 workdir 안으로 제한해 정규화한다(read/edit/grep/glob base).
///
/// canonicalize 는 심볼릭 링크까지 실제 경로로 풀어주므로, 그 결과가 workdir(역시
/// canonicalize) 하위인지 검사하면 링크를 통한 탈출도 막힌다.
pub(crate) fn confine_existing(workdir: &Path, rel: &str) -> Result<PathBuf, String> {
    let canon_workdir = canon_workdir(workdir)?;
    let canon = workdir
        .join(rel)
        .canonicalize()
        .map_err(|e| format!("invalid path `{rel}`: {e}"))?;
    if canon.starts_with(&canon_workdir) {
        Ok(canon)
    } else {
        Err(format!("path `{rel}` escapes workspace root"))
    }
}

/// **새로 만들** 경로를 workdir 안으로 제한한다(write). 대상 파일은 아직 없어도 되지만,
/// 그 **부모 디렉터리는 존재**하고 workdir 안이어야 한다(새 중첩 디렉터리 생성은 막는다).
pub(crate) fn confine_new(workdir: &Path, rel: &str) -> Result<PathBuf, String> {
    let canon_workdir = canon_workdir(workdir)?;
    let target = workdir.join(rel);
    let parent = target
        .parent()
        .ok_or_else(|| format!("path `{rel}` has no parent directory"))?;
    let file_name = target
        .file_name()
        .ok_or_else(|| format!("path `{rel}` does not name a file"))?;
    let canon_parent = parent
        .canonicalize()
        .map_err(|e| format!("parent directory of `{rel}` not found: {e}"))?;
    if canon_parent.starts_with(&canon_workdir) {
        Ok(canon_parent.join(file_name))
    } else {
        Err(format!("path `{rel}` escapes workspace root"))
    }
}

fn canon_workdir(workdir: &Path) -> Result<PathBuf, String> {
    workdir
        .canonicalize()
        .map_err(|e| format!("invalid workspace root: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("scv-path-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir.canonicalize().expect("canonicalize temp dir")
    }

    #[test]
    fn confine_existing_accepts_in_workdir_and_rejects_escape() {
        let wd = temp_dir("existing");
        std::fs::write(wd.join("a.txt"), "x").unwrap();
        assert!(confine_existing(&wd, "a.txt").is_ok());
        // `..` 탈출은 거부.
        assert!(confine_existing(&wd, "../etc-passwd-like").is_err());
        // 절대경로 탈출도 거부.
        assert!(confine_existing(&wd, "/etc/hosts").is_err());
        let _ = std::fs::remove_dir_all(&wd);
    }

    #[test]
    fn confine_new_allows_new_file_in_existing_dir_only() {
        let wd = temp_dir("new");
        std::fs::create_dir_all(wd.join("sub")).unwrap();
        // 존재하는 디렉터리 안의 새 파일 → OK.
        let resolved = confine_new(&wd, "sub/new.txt").expect("ok");
        assert!(resolved.starts_with(&wd));
        assert!(resolved.ends_with("new.txt"));
        // 부모 디렉터리가 없으면 거부(새 중첩 디렉터리 생성 금지).
        assert!(confine_new(&wd, "missing/new.txt").is_err());
        // 탈출 거부.
        assert!(confine_new(&wd, "../escape.txt").is_err());
        let _ = std::fs::remove_dir_all(&wd);
    }
}
