//! `read` 도구 — 파일을 읽는다(읽기 전용, 병렬 안전).
//!
//! 다른 내장 도구(`write`/`edit`/`bash`/`glob`/`grep`)도 같은 패턴을 따른다.

use async_trait::async_trait;
use scv_core::tool::{PermissionLevel, Tool, ToolContext, ToolOutput};

#[derive(Debug)]
pub struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file from the workspace. Use this before editing a file."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Workspace-relative file path" }
            },
            "required": ["path"],
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
        let Some(rel) = input.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::error("missing `path`");
        };
        // 보안: 경로를 workdir 안으로 제한한다(.. 탈출/심볼릭 링크 방지).
        let path = match crate::path::confine_existing(&ctx.workdir, rel) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(e),
        };
        match tokio::fs::read_to_string(&path).await {
            Ok(text) => ToolOutput::ok(text),
            Err(e) => ToolOutput::error(format!("read failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scv_core::tool::CancellationToken;

    fn ctx(tag: &str) -> ToolContext {
        let dir = std::env::temp_dir().join(format!("scv-read-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        ToolContext {
            workdir: dir.canonicalize().expect("canon"),
            cancel: CancellationToken::new(),
        }
    }

    #[test]
    fn metadata_is_read_only_and_parallel_safe() {
        assert_eq!(ReadTool.name(), "read");
        assert!(!ReadTool.description().is_empty());
        assert_eq!(ReadTool.input_schema()["type"], "object");
        assert_eq!(
            ReadTool.permission(&serde_json::json!({})),
            PermissionLevel::Allow
        );
        assert!(ReadTool.parallel_safe());
    }

    #[tokio::test]
    async fn reads_an_existing_file() {
        let ctx = ctx("ok");
        std::fs::write(ctx.workdir.join("a.txt"), "hello\nworld").unwrap();
        let out = ReadTool
            .invoke(serde_json::json!({ "path": "a.txt" }), &ctx)
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert_eq!(out.content, "hello\nworld");
        let _ = std::fs::remove_dir_all(&ctx.workdir);
    }

    #[tokio::test]
    async fn missing_path_is_error() {
        let ctx = ctx("missing");
        let out = ReadTool.invoke(serde_json::json!({}), &ctx).await;
        assert!(out.is_error);
        assert!(out.content.contains("missing `path`"));
        let _ = std::fs::remove_dir_all(&ctx.workdir);
    }

    #[tokio::test]
    async fn path_escape_is_rejected() {
        let ctx = ctx("escape");
        let out = ReadTool
            .invoke(serde_json::json!({ "path": "../../etc/hosts" }), &ctx)
            .await;
        assert!(out.is_error);
        let _ = std::fs::remove_dir_all(&ctx.workdir);
    }

    #[tokio::test]
    async fn nonexistent_path_is_error() {
        // 존재하지 않는 경로는 confine_existing(canonicalize)에서 먼저 거부된다.
        let ctx = ctx("absent");
        let out = ReadTool
            .invoke(serde_json::json!({ "path": "nope.txt" }), &ctx)
            .await;
        assert!(out.is_error);
        let _ = std::fs::remove_dir_all(&ctx.workdir);
    }

    #[tokio::test]
    async fn reading_a_directory_reports_read_failure() {
        // 경로가 workdir 안에 존재하지만 디렉터리면 read_to_string 이 실패한다.
        let ctx = ctx("isdir");
        std::fs::create_dir(ctx.workdir.join("sub")).unwrap();
        let out = ReadTool
            .invoke(serde_json::json!({ "path": "sub" }), &ctx)
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("read failed"), "{}", out.content);
        let _ = std::fs::remove_dir_all(&ctx.workdir);
    }
}
