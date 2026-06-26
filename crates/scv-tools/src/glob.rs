//! `glob` 도구 — 패턴으로 워크스페이스 파일을 찾는다(읽기 전용, 병렬 안전).
//!
//! `.gitignore` 를 존중하며(`ignore` 크레이트), 경로는 `workdir` 안으로 제한한다.
//! 부작용이 없어 `Allow` + `parallel_safe`.

use std::path::Path;

use async_trait::async_trait;
use globset::GlobBuilder;
use ignore::WalkBuilder;
use scv_core::tool::{PermissionLevel, Tool, ToolContext, ToolOutput};

/// 한 번에 돌려줄 최대 매치 수(컨텍스트 폭주 방지).
const MAX_MATCHES: usize = 1000;

#[derive(Debug)]
pub struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files by glob pattern (e.g. \"**/*.rs\"), respecting .gitignore. \
         Returns workspace-relative paths."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob, e.g. \"src/**/*.rs\"" },
                "path": { "type": "string", "description": "Optional base dir (workspace-relative). Default: workspace root." }
            },
            "required": ["pattern"],
            "additionalProperties": false
        })
    }

    fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
        PermissionLevel::Allow
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    async fn invoke(&self, input: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) else {
            return ToolOutput::error("missing `pattern`");
        };
        let base_rel = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let base = match crate::path::confine_existing(&ctx.workdir, base_rel) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(e),
        };

        // 파일시스템 walk 는 블로킹이므로 별도 스레드로(async 런타임 차단 방지).
        let workdir = ctx.workdir.clone();
        let pattern = pattern.to_string();
        let joined = tokio::task::spawn_blocking(move || walk(&workdir, &base, &pattern)).await;
        match joined {
            Ok(Ok(paths)) if paths.is_empty() => ToolOutput::ok("(no matches)"),
            Ok(Ok(paths)) => ToolOutput::ok(paths.join("\n")),
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("glob task failed: {e}")),
        }
    }
}

/// `base` 아래를 `.gitignore` 존중하며 훑어 `pattern` 에 맞는 workdir-상대 경로를 모은다.
fn walk(workdir: &Path, base: &Path, pattern: &str) -> Result<Vec<String>, String> {
    let matcher = GlobBuilder::new(pattern)
        .literal_separator(true) // `*` 가 `/` 를 넘지 않게 — `**` 라야 재귀
        .build()
        .map_err(|e| format!("invalid glob `{pattern}`: {e}"))?
        .compile_matcher();
    let canon_workdir = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());

    let mut out = Vec::new();
    for entry in WalkBuilder::new(base).build() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&canon_workdir)
            .unwrap_or(entry.path());
        if matcher.is_match(rel) {
            out.push(rel.to_string_lossy().into_owned());
            if out.len() >= MAX_MATCHES {
                break;
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("scv-glob-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        dir.canonicalize().expect("canon")
    }

    #[test]
    fn matches_recursive_pattern_and_sorts() {
        let wd = temp_workspace("match");
        std::fs::write(wd.join("src/a.rs"), "").unwrap();
        std::fs::write(wd.join("src/b.rs"), "").unwrap();
        std::fs::write(wd.join("README.md"), "").unwrap();

        let found = walk(&wd, &wd, "**/*.rs").expect("walk ok");
        assert_eq!(found, vec!["src/a.rs".to_string(), "src/b.rs".to_string()]);

        let _ = std::fs::remove_dir_all(&wd);
    }

    #[test]
    fn literal_separator_blocks_single_star_crossing_dirs() {
        let wd = temp_workspace("sep");
        std::fs::write(wd.join("src/a.rs"), "").unwrap();
        // `*.rs`(루트 직속만) 는 src/a.rs 에 매치되지 않아야 한다.
        let found = walk(&wd, &wd, "*.rs").expect("walk ok");
        assert!(found.is_empty(), "found = {found:?}");
        let _ = std::fs::remove_dir_all(&wd);
    }
}
