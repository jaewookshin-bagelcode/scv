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
