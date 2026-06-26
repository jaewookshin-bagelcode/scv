//! `write` 도구 — 파일에 내용을 쓴다(새로 만들거나 덮어씀). **비가역 → `Ask`**.
//!
//! 경로는 `workdir` 안으로 제한한다. 기존 경로(심볼릭 링크 포함)는 실제 경로가 workdir
//! 안인지까지 검증하고, 새 파일은 부모 디렉터리가 workdir 안에 존재해야 한다.

use async_trait::async_trait;
use scv_core::tool::{PermissionLevel, Tool, ToolContext, ToolOutput};

#[derive(Debug)]
pub struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Write (create or overwrite) a UTF-8 text file in the workspace. \
         The parent directory must already exist."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Workspace-relative file path" },
                "content": { "type": "string", "description": "Full file contents to write" }
            },
            "required": ["path", "content"],
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
        let Some(content) = input.get("content").and_then(|v| v.as_str()) else {
            return ToolOutput::error("missing `content`");
        };

        // 이미 존재(심볼릭 링크 포함)하면 실제 경로 검증, 없으면 새 파일 경로 검증.
        let exists = ctx.workdir.join(rel).symlink_metadata().is_ok();
        let resolved = if exists {
            crate::path::confine_existing(&ctx.workdir, rel)
        } else {
            crate::path::confine_new(&ctx.workdir, rel)
        };
        let path = match resolved {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(e),
        };

        match tokio::fs::write(&path, content).await {
            Ok(()) => ToolOutput::ok(format!("wrote {} bytes to {rel}", content.len())),
            Err(e) => ToolOutput::error(format!("write failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scv_core::tool::CancellationToken;

    fn ctx(tag: &str) -> ToolContext {
        let dir = std::env::temp_dir().join(format!("scv-write-{}-{}", std::process::id(), tag));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        ToolContext {
            workdir: dir.canonicalize().expect("canon"),
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn writes_a_new_file() {
        let ctx = ctx("new");
        let out = WriteTool
            .invoke(
                serde_json::json!({ "path": "hello.txt", "content": "hi there" }),
                &ctx,
            )
            .await;
        assert!(!out.is_error, "{}", out.content);
        let written = std::fs::read_to_string(ctx.workdir.join("hello.txt")).unwrap();
        assert_eq!(written, "hi there");
        let _ = std::fs::remove_dir_all(&ctx.workdir);
    }

    #[tokio::test]
    async fn rejects_escape_via_parent() {
        let ctx = ctx("escape");
        let out = WriteTool
            .invoke(
                serde_json::json!({ "path": "../escaped.txt", "content": "x" }),
                &ctx,
            )
            .await;
        assert!(out.is_error);
        assert!(!ctx.workdir.parent().unwrap().join("escaped.txt").exists());
        let _ = std::fs::remove_dir_all(&ctx.workdir);
    }
}
