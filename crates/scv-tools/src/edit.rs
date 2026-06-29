//! `edit` 도구 — 파일에서 정확히 일치하는 문자열을 치환한다. **비가역 → `Ask`**.
//!
//! 기본은 유일 일치(안전). 여러 곳을 바꾸려면 `replace_all: true`. 모델이 먼저 `read`
//! 로 파일을 본 뒤 편집하는 흐름을 전제한다. 경로는 `workdir` 안으로 제한한다.

use async_trait::async_trait;
use scv_core::tool::{PermissionLevel, Tool, ToolContext, ToolOutput};

#[derive(Debug)]
pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace an exact string in an existing workspace file. By default `old_string` must \
         be unique; pass replace_all=true to replace every occurrence. Read the file first."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Workspace-relative file path" },
                "old_string": { "type": "string", "description": "Exact text to replace" },
                "new_string": { "type": "string", "description": "Replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace all occurrences (default false)" }
            },
            "required": ["path", "old_string", "new_string"],
            "additionalProperties": false
        })
    }

    fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
        PermissionLevel::Ask
    }

    async fn invoke(&self, input: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(rel) = input.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::error("missing `path`");
        };
        let Some(old) = input.get("old_string").and_then(|v| v.as_str()) else {
            return ToolOutput::error("missing `old_string`");
        };
        let Some(new) = input.get("new_string").and_then(|v| v.as_str()) else {
            return ToolOutput::error("missing `new_string`");
        };
        let replace_all = input
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let path = match crate::path::confine_existing(&ctx.workdir, rel) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(e),
        };
        let content = match tokio::fs::read_to_string(&path).await {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("read failed: {e}")),
        };
        let (updated, count) = match apply_edit(&content, old, new, replace_all) {
            Ok(result) => result,
            Err(e) => return ToolOutput::error(e),
        };
        match tokio::fs::write(&path, updated).await {
            Ok(()) => ToolOutput::ok(format!("edited {rel}: {count} replacement(s)")),
            Err(e) => ToolOutput::error(format!("write failed: {e}")),
        }
    }
}

/// 순수 치환 로직(IO 없음 → 단위 테스트 용이). 치환된 문자열과 횟수를 돌려준다.
fn apply_edit(
    content: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, usize), String> {
    if old.is_empty() {
        return Err("`old_string` must not be empty".into());
    }
    if old == new {
        return Err("`old_string` and `new_string` are identical".into());
    }
    let count = content.matches(old).count();
    match count {
        0 => Err("`old_string` not found in file".into()),
        n if n > 1 && !replace_all => Err(format!(
            "`old_string` is not unique ({n} matches); add surrounding context or set replace_all=true"
        )),
        _ => {
            let updated = if replace_all {
                content.replace(old, new)
            } else {
                content.replacen(old, new, 1)
            };
            Ok((updated, count))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_unique_match() {
        let (out, n) = apply_edit("let x = 1;", "1", "2", false).unwrap();
        assert_eq!(out, "let x = 2;");
        assert_eq!(n, 1);
    }

    #[test]
    fn rejects_ambiguous_without_replace_all() {
        assert!(apply_edit("a a a", "a", "b", false).is_err());
    }

    #[test]
    fn replace_all_replaces_every_occurrence() {
        let (out, n) = apply_edit("a a a", "a", "b", true).unwrap();
        assert_eq!(out, "b b b");
        assert_eq!(n, 3);
    }

    #[test]
    fn rejects_missing_and_identical_and_empty() {
        assert!(apply_edit("hello", "zzz", "y", false).is_err());
        assert!(apply_edit("hello", "h", "h", false).is_err());
        assert!(apply_edit("hello", "", "x", false).is_err());
    }

    fn ctx(tag: &str) -> ToolContext {
        let dir = std::env::temp_dir().join(format!("scv-edit-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        ToolContext {
            workdir: dir.canonicalize().expect("canon"),
            cancel: scv_core::tool::CancellationToken::new(),
        }
    }

    #[test]
    fn metadata_is_ask() {
        assert_eq!(EditTool.name(), "edit");
        assert!(!EditTool.description().is_empty());
        assert_eq!(EditTool.input_schema()["type"], "object");
        assert_eq!(
            EditTool.permission(&serde_json::json!({})),
            PermissionLevel::Ask
        );
        assert!(!EditTool.parallel_safe());
    }

    #[tokio::test]
    async fn invoke_edits_a_real_file() {
        let ctx = ctx("ok");
        std::fs::write(ctx.workdir.join("f.txt"), "let x = 1;").unwrap();
        let out = EditTool
            .invoke(
                serde_json::json!({ "path": "f.txt", "old_string": "1", "new_string": "2" }),
                &ctx,
            )
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("1 replacement"));
        let after = std::fs::read_to_string(ctx.workdir.join("f.txt")).unwrap();
        assert_eq!(after, "let x = 2;");
        let _ = std::fs::remove_dir_all(&ctx.workdir);
    }

    #[tokio::test]
    async fn invoke_rejects_missing_fields() {
        let ctx = ctx("missing");
        assert!(EditTool
            .invoke(serde_json::json!({}), &ctx)
            .await
            .content
            .contains("missing `path`"));
        assert!(EditTool
            .invoke(serde_json::json!({ "path": "f" }), &ctx)
            .await
            .content
            .contains("missing `old_string`"));
        assert!(EditTool
            .invoke(serde_json::json!({ "path": "f", "old_string": "a" }), &ctx,)
            .await
            .content
            .contains("missing `new_string`"));
        let _ = std::fs::remove_dir_all(&ctx.workdir);
    }

    #[tokio::test]
    async fn invoke_reports_read_failure_and_apply_error() {
        let ctx = ctx("errors");
        // 디렉터리 경로 → confine 은 통과하지만 read_to_string 이 실패(read failed 분기).
        std::fs::create_dir(ctx.workdir.join("sub")).unwrap();
        let read_fail = EditTool
            .invoke(
                serde_json::json!({ "path": "sub", "old_string": "a", "new_string": "b" }),
                &ctx,
            )
            .await;
        assert!(read_fail.is_error);
        assert!(
            read_fail.content.contains("read failed"),
            "{}",
            read_fail.content
        );

        // 존재하지만 old_string 이 없음 → apply_edit 에러.
        std::fs::write(ctx.workdir.join("g.txt"), "hello").unwrap();
        let not_found = EditTool
            .invoke(
                serde_json::json!({ "path": "g.txt", "old_string": "zzz", "new_string": "b" }),
                &ctx,
            )
            .await;
        assert!(not_found.is_error);
        let _ = std::fs::remove_dir_all(&ctx.workdir);
    }
}
