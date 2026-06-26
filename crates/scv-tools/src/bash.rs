//! `bash` 도구 — `sh -c` 로 명령을 실행한다. **비가역·임의 실행 → `Ask`**.
//!
//! 보안(CODING_RULES §8): 명령 문자열은 **신뢰 불가 모델 출력**이다. 그래서 `Ask` 로
//! 게이팅하고(fail-closed — 모달/명시적 Allow 없이는 실행 안 됨), `workdir` 에서 돌리며,
//! 타임아웃으로 무한 실행을 막는다. 자식 프로세스는 `kill_on_drop` 으로 정리한다.
//!
//! 취소: 실행 중 `ctx.cancel` 이 켜지면 `tokio::select!` 로 즉시 빠져나오고, 자식은
//! `kill_on_drop` 으로 정리된다(타임아웃과 함께 두 겹의 경계 — ARCHITECTURE §4.3 취소 협조).

use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use async_trait::async_trait;
use scv_core::tool::{PermissionLevel, Tool, ToolContext, ToolOutput};

const DEFAULT_TIMEOUT_MS: u64 = 120_000;
const MAX_TIMEOUT_MS: u64 = 600_000;
/// 도구 결과로 돌려줄 최대 출력 길이(컨텍스트 폭주 방지).
const MAX_OUTPUT: usize = 30_000;

#[derive(Debug)]
pub struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Run a shell command (sh -c) in the workspace directory and return its combined \
         stdout/stderr and exit code. Use dedicated tools (read/write/edit/glob/grep) when they fit."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run" },
                "timeout_ms": { "type": "integer", "description": "Timeout in ms (default 120000, max 600000)" }
            },
            "required": ["command"],
            "additionalProperties": false
        })
    }

    fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
        PermissionLevel::Ask
    }

    async fn invoke(&self, input: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(command) = input.get("command").and_then(|v| v.as_str()) else {
            return ToolOutput::error("missing `command`");
        };
        if command.trim().is_empty() {
            return ToolOutput::error("empty `command`");
        }
        let timeout_ms = input
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_MS)
            .min(MAX_TIMEOUT_MS);

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&ctx.workdir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return ToolOutput::error(format!("failed to spawn command: {e}")),
        };

        // 협조적 취소: 취소 신호·타임아웃을 자식 대기와 경쟁시킨다. 어느 쪽이 이기든
        // wait 미래가 드롭되고 `kill_on_drop` 이 자식을 정리한다.
        let wait =
            tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output());
        tokio::select! {
            _ = ctx.cancel.cancelled() => ToolOutput::error("command cancelled"),
            res = wait => match res {
                Err(_elapsed) => ToolOutput::error(format!("command timed out after {timeout_ms}ms")),
                Ok(Err(e)) => ToolOutput::error(format!("command failed: {e}")),
                Ok(Ok(output)) => {
                    let text = format_output(&output.stdout, &output.stderr, output.status);
                    if output.status.success() {
                        ToolOutput::ok(text)
                    } else {
                        ToolOutput::error(text)
                    }
                }
            }
        }
    }
}

/// stdout/stderr/exit 코드를 사람이 읽을 한 덩어리로 합치고 길이를 제한한다.
fn format_output(stdout: &[u8], stderr: &[u8], status: ExitStatus) -> String {
    let out = String::from_utf8_lossy(stdout);
    let err = String::from_utf8_lossy(stderr);
    let mut s = String::new();
    if !out.trim().is_empty() {
        s.push_str(out.trim_end());
        s.push('\n');
    }
    if !err.trim().is_empty() {
        s.push_str("[stderr]\n");
        s.push_str(err.trim_end());
        s.push('\n');
    }
    let code = status
        .code()
        .map_or_else(|| "killed by signal".to_string(), |c| c.to_string());
    s.push_str(&format!("[exit: {code}]"));
    truncate(s)
}

fn truncate(mut s: String) -> String {
    if s.len() <= MAX_OUTPUT {
        return s;
    }
    // 문자 경계까지 잘라낸다.
    let mut cut = MAX_OUTPUT;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str("\n…(output truncated)");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use scv_core::tool::CancellationToken;

    fn ctx() -> ToolContext {
        ToolContext {
            workdir: std::env::temp_dir(),
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn runs_command_and_captures_stdout() {
        let out = BashTool
            .invoke(serde_json::json!({ "command": "echo hi" }), &ctx())
            .await;
        assert!(!out.is_error, "{}", out.content);
        assert!(out.content.contains("hi"), "{}", out.content);
        assert!(out.content.contains("[exit: 0]"));
    }

    #[tokio::test]
    async fn nonzero_exit_is_marked_error() {
        let out = BashTool
            .invoke(serde_json::json!({ "command": "exit 3" }), &ctx())
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("[exit: 3]"), "{}", out.content);
    }

    #[tokio::test]
    async fn timeout_is_reported() {
        let out = BashTool
            .invoke(
                serde_json::json!({ "command": "sleep 5", "timeout_ms": 100 }),
                &ctx(),
            )
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("timed out"), "{}", out.content);
    }
}
