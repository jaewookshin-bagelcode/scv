//! `web_fetch` 도구 — URL 을 HTTP(S) GET 으로 가져와 본문 텍스트를 돌려준다.
//!
//! **네트워크 egress** 라 권한은 `Ask`(매번 승인). 부작용이 외부 호출뿐이라 `parallel_safe`.
//! 응답은 길이를 제한해 돌려준다(컨텍스트 폭주 방지). 취소 신호를 관찰해 즉시 중단한다.

use async_trait::async_trait;
use scv_core::tool::{PermissionLevel, Tool, ToolContext, ToolOutput};

/// 돌려줄 본문 최대 길이(문자). 초과분은 잘라낸다.
const MAX_BODY_CHARS: usize = 100_000;

#[derive(Debug)]
pub struct WebFetchTool {
    http: reqwest::Client,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch a URL over HTTP(S) GET and return the response body as text (truncated). \
         Network egress — requires approval."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "Absolute http(s) URL to GET" }
            },
            "required": ["url"],
            "additionalProperties": false
        })
    }

    fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
        // 네트워크 egress 는 되돌릴 수 없는(외부에 흔적이 남는) 동작 → 승인 필요.
        PermissionLevel::Ask
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    async fn invoke(&self, input: serde_json::Value, ctx: &ToolContext) -> ToolOutput {
        let Some(url) = input.get("url").and_then(|v| v.as_str()) else {
            return ToolOutput::error("missing `url`");
        };
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return ToolOutput::error("url must start with http:// or https://");
        }

        // 취소 신호를 관찰해 긴 요청을 즉시 중단한다(§4.5 협조적 취소).
        let send = self.http.get(url).send();
        let resp = tokio::select! {
            _ = ctx.cancel.cancelled() => return ToolOutput::error("cancelled"),
            r = send => r,
        };
        let resp = match resp {
            Ok(r) => r,
            Err(e) => return ToolOutput::error(format!("fetch failed: {e}")),
        };
        let status = resp.status();
        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => return ToolOutput::error(format!("reading body failed: {e}")),
        };
        let truncated = truncate(&body, MAX_BODY_CHARS);
        if status.is_success() {
            ToolOutput::ok(truncated)
        } else {
            // 비-2xx 는 본문째 에러로 — 모델이 사유를 보고 복구할 수 있게.
            ToolOutput::error(format!("HTTP {status}\n{truncated}"))
        }
    }
}

/// `max` 문자에서 자르고 잘렸음을 표시한다.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}\n[truncated at {max} chars]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use scv_core::tool::{CancellationToken, ToolContext};

    fn ctx() -> ToolContext {
        ToolContext {
            workdir: std::env::temp_dir(),
            cancel: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn rejects_missing_or_non_http_url() {
        let tool = WebFetchTool::new();
        assert!(tool.invoke(serde_json::json!({}), &ctx()).await.is_error);
        let out = tool
            .invoke(serde_json::json!({ "url": "ftp://x/y" }), &ctx())
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("http"));
    }

    #[tokio::test]
    async fn cancelled_before_fetch_returns_error() {
        let tool = WebFetchTool::new();
        let c = ctx();
        c.cancel.cancel();
        let out = tool
            .invoke(serde_json::json!({ "url": "http://example.invalid" }), &c)
            .await;
        assert!(out.is_error);
    }

    #[test]
    fn permission_is_ask_and_parallel_safe() {
        let tool = WebFetchTool::new();
        assert_eq!(
            tool.permission(&serde_json::json!({})),
            PermissionLevel::Ask
        );
        assert!(tool.parallel_safe());
    }

    #[test]
    fn truncate_marks_when_cut() {
        let s = "a".repeat(10);
        assert_eq!(truncate(&s, 100), s);
        let out = truncate(&s, 4);
        assert!(out.starts_with("aaaa"));
        assert!(out.contains("truncated"));
    }
}
