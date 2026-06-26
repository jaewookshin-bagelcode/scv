//! `grep` 도구 — 정규식으로 파일 내용을 검색한다(읽기 전용, 병렬 안전).
//!
//! `.gitignore` 존중, 경로는 `workdir` 안으로 제한, UTF-8 아닌(이진) 파일은 건너뛴다.
//! 부작용이 없어 `Allow` + `parallel_safe`.

use std::path::Path;

use async_trait::async_trait;
use globset::GlobBuilder;
use ignore::WalkBuilder;
use regex::Regex;
use scv_core::tool::{PermissionLevel, Tool, ToolContext, ToolOutput};

/// 한 번에 돌려줄 최대 매치 라인 수.
const MAX_MATCHES: usize = 200;
/// 한 매치 라인의 최대 출력 길이(긴 줄 잘라내기).
const MAX_LINE_LEN: usize = 300;

#[derive(Debug)]
pub struct GrepTool;

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search file contents by regular expression, respecting .gitignore. \
         Returns lines as \"path:lineno:text\". Optionally filter files by a glob."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Rust regex to search for" },
                "path": { "type": "string", "description": "Optional base dir (workspace-relative). Default: workspace root." },
                "glob": { "type": "string", "description": "Optional file glob filter, e.g. \"**/*.rs\"" }
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
        let file_glob = input
            .get("glob")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let base = match crate::path::confine_existing(&ctx.workdir, base_rel) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(e),
        };

        let workdir = ctx.workdir.clone();
        let pattern = pattern.to_string();
        let joined = tokio::task::spawn_blocking(move || {
            search(&workdir, &base, &pattern, file_glob.as_deref())
        })
        .await;
        match joined {
            Ok(Ok(lines)) if lines.is_empty() => ToolOutput::ok("(no matches)"),
            Ok(Ok(lines)) => ToolOutput::ok(lines.join("\n")),
            Ok(Err(e)) => ToolOutput::error(e),
            Err(e) => ToolOutput::error(format!("grep task failed: {e}")),
        }
    }
}

/// `base` 아래 파일들에서 `regex` 매치 라인을 `path:lineno:text` 로 모은다.
fn search(
    workdir: &Path,
    base: &Path,
    pattern: &str,
    file_glob: Option<&str>,
) -> Result<Vec<String>, String> {
    let re = Regex::new(pattern).map_err(|e| format!("invalid regex `{pattern}`: {e}"))?;
    let glob = match file_glob {
        Some(g) => Some(
            GlobBuilder::new(g)
                .literal_separator(true)
                .build()
                .map_err(|e| format!("invalid glob `{g}`: {e}"))?
                .compile_matcher(),
        ),
        None => None,
    };
    let canon_workdir = workdir
        .canonicalize()
        .unwrap_or_else(|_| workdir.to_path_buf());

    let mut out = Vec::new();
    'walk: for entry in WalkBuilder::new(base).build() {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(&canon_workdir)
            .unwrap_or(entry.path());
        if glob.as_ref().is_some_and(|g| !g.is_match(rel)) {
            continue;
        }
        // 이진/비-UTF8 파일은 조용히 건너뛴다(grep 은 텍스트 검색).
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let rel = rel.to_string_lossy();
        for (lineno, line) in text.lines().enumerate() {
            if re.is_match(line) {
                out.push(format!("{rel}:{}:{}", lineno + 1, truncate(line)));
                if out.len() >= MAX_MATCHES {
                    break 'walk;
                }
            }
        }
    }
    Ok(out)
}

/// 긴 라인을 표시용으로 잘라낸다(문자 경계 기준).
fn truncate(line: &str) -> String {
    if line.len() <= MAX_LINE_LEN {
        return line.to_string();
    }
    let cut = line
        .char_indices()
        .take_while(|(i, _)| *i < MAX_LINE_LEN)
        .last()
        .map_or(0, |(i, c)| i + c.len_utf8());
    format!("{}…", &line[..cut])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_workspace(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("scv-grep-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).expect("mkdir");
        dir.canonicalize().expect("canon")
    }

    #[test]
    fn finds_regex_matches_with_line_numbers() {
        let wd = temp_workspace("find");
        std::fs::write(wd.join("src/a.rs"), "fn alpha() {}\nfn beta() {}\n").unwrap();
        let lines = search(&wd, &wd, r"fn \w+", None).expect("ok");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("src/a.rs:1:"));
        assert!(lines[1].starts_with("src/a.rs:2:"));
        let _ = std::fs::remove_dir_all(&wd);
    }

    #[test]
    fn glob_filter_restricts_files() {
        let wd = temp_workspace("filter");
        std::fs::write(wd.join("src/a.rs"), "needle\n").unwrap();
        std::fs::write(wd.join("notes.md"), "needle\n").unwrap();
        let only_rs = search(&wd, &wd, "needle", Some("**/*.rs")).expect("ok");
        assert_eq!(only_rs.len(), 1);
        assert!(only_rs[0].starts_with("src/a.rs:1:"));
        let _ = std::fs::remove_dir_all(&wd);
    }

    #[test]
    fn invalid_regex_is_reported() {
        let wd = temp_workspace("badre");
        assert!(search(&wd, &wd, "(unclosed", None).is_err());
        let _ = std::fs::remove_dir_all(&wd);
    }
}
