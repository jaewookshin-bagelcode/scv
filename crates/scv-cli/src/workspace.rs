//! 세션별 격리 작업공간(ARCHITECTURE §4.2 세션 격리).
//!
//! 도구의 파일 읽기/쓰기 루트(`ToolContext.workdir`)가 세션 간 격리의 **유일한 경계**다.
//! 같은 repo 에서 여러 세션이 동시에 돌면 같은 파일을 건드려 충돌할 수 있으므로, `--isolate`
//! 시 세션마다 **별도 git worktree**(같은 커밋의 독립 체크아웃)를 만들어 그 경로를 workdir
//! 로 준다. 세션이 끝나면 `Drop` 에서 worktree 를 제거한다.
//!
//! git repo 가 아니거나 worktree 생성에 실패하면 격리 없이 cwd 를 그대로 쓴다(경고).
//! 합성 루트(scv-cli)에 두는 이유: `git` 서브프로세스 + 파일시스템 부작용을 다루기 때문.

use std::path::{Path, PathBuf};
use std::process::Command;

/// 한 세션의 작업공간. worktree 를 만들었으면 `Drop` 에서 정리한다.
#[derive(Debug)]
pub struct SessionWorkspace {
    path: PathBuf,
    /// worktree 를 만든 경우 그 repo 루트(정리에 사용). cwd 폴백이면 None.
    cleanup_repo: Option<PathBuf>,
}

impl SessionWorkspace {
    /// cwd 기준으로 세션 작업공간을 만든다. `isolate=false` 면 cwd 를 그대로 쓴다(격리 없음).
    pub fn create(cwd: &Path, session_id: &str, isolate: bool) -> Self {
        Self::create_in(cwd, &default_worktrees_dir(), session_id, isolate)
    }

    /// 격리에 쓸 작업 디렉터리 경로(도구 workdir 로 주입).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `worktrees_root` 아래에 세션 worktree 를 만든다(테스트가 루트를 주입할 수 있게 분리).
    fn create_in(cwd: &Path, worktrees_root: &Path, session_id: &str, isolate: bool) -> Self {
        let fallback = || SessionWorkspace {
            path: cwd.to_path_buf(),
            cleanup_repo: None,
        };
        if !isolate {
            return fallback();
        }
        let Some(repo_root) = git_repo_root(cwd) else {
            tracing::warn!("--isolate: cwd 가 git repo 가 아니라 격리 없이 cwd 를 쓴다");
            return fallback();
        };
        let worktree = worktrees_root.join(session_id);
        let worktree_str = worktree.to_string_lossy().to_string();
        match git(
            &repo_root,
            &["worktree", "add", "--detach", &worktree_str, "HEAD"],
        ) {
            Ok(out) if out.status.success() => {
                tracing::info!(path = %worktree_str, "세션 worktree 생성");
                SessionWorkspace {
                    path: worktree,
                    cleanup_repo: Some(repo_root),
                }
            }
            other => {
                tracing::warn!(?other, "git worktree add 실패 → 격리 없이 cwd 사용");
                fallback()
            }
        }
    }
}

impl Drop for SessionWorkspace {
    fn drop(&mut self) {
        if let Some(repo) = self.cleanup_repo.take() {
            let p = self.path.to_string_lossy().to_string();
            // 실패해도 할 수 있는 게 없다(로그만) — 사용자가 `git worktree prune` 으로 정리 가능.
            if let Err(e) = git(&repo, &["worktree", "remove", "--force", &p]) {
                tracing::warn!(error = %e, path = %p, "worktree 제거 실패(수동 정리 필요)");
            }
        }
    }
}

/// `~/.scv/worktrees`(세션 worktree 보관소).
fn default_worktrees_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    home.join(".scv/worktrees")
}

/// `dir` 가 속한 git repo 루트(아니면 None).
fn git_repo_root(dir: &Path) -> Option<PathBuf> {
    let out = git(dir, &["rev-parse", "--show-toplevel"]).ok()?;
    if !out.status.success() {
        return None;
    }
    let root = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!root.is_empty()).then(|| PathBuf::from(root))
}

fn git(dir: &Path, args: &[&str]) -> std::io::Result<std::process::Output> {
    Command::new("git").current_dir(dir).args(args).output()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git_available() -> bool {
        Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn run(dir: &Path, args: &[&str]) {
        let out = git(dir, args).expect("git runs");
        assert!(out.status.success(), "git {args:?} failed: {out:?}");
    }

    #[test]
    fn isolate_false_uses_cwd() {
        let cwd = std::env::temp_dir();
        let ws = SessionWorkspace::create_in(&cwd, &cwd.join("wt"), "s1", false);
        assert_eq!(ws.path(), cwd);
    }

    #[test]
    fn non_git_dir_falls_back_to_cwd() {
        let base = std::env::temp_dir().join(format!("scv-wt-nongit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let ws = SessionWorkspace::create_in(&base, &base.join("wt"), "s1", true);
        // git repo 가 아니므로 cwd 폴백.
        assert_eq!(ws.path(), base);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn worktree_created_and_removed_on_drop() {
        if !git_available() {
            eprintln!("skip: git not available");
            return;
        }
        let base = std::env::temp_dir().join(format!("scv-wt-repo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // 임시 git repo + 커밋 1개.
        run(&base, &["init", "-q"]);
        std::fs::write(base.join("file.txt"), "hello").unwrap();
        run(&base, &["add", "."]);
        run(
            &base,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-q",
                "-m",
                "init",
            ],
        );

        let worktrees = base.join("worktrees");
        let path = {
            let ws = SessionWorkspace::create_in(&base, &worktrees, "sess1", true);
            // 격리된 별도 경로 + 커밋된 파일이 체크아웃돼 있어야 한다.
            assert_ne!(ws.path(), base.as_path());
            assert!(ws.path().join("file.txt").exists(), "worktree has the file");
            ws.path().to_path_buf()
        }; // 여기서 Drop → worktree 제거.
        assert!(!path.exists(), "worktree dir removed on drop");

        let _ = std::fs::remove_dir_all(&base);
    }
}
