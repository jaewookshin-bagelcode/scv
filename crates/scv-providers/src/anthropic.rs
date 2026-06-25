//! Anthropic Messages API 어댑터.
//!
//! 공식 Rust SDK 가 없으므로 `reqwest` 로 `POST /v1/messages` 를 직접 호출하고,
//! `eventsource-stream` 으로 SSE 응답을 파싱한다.
//!
//! 와이어 사양(요지):
//! - 엔드포인트: `POST {base_url}/v1/messages`
//! - 헤더: `x-api-key`, `anthropic-version: 2023-06-01`, `content-type: application/json`
//! - 본문(요청): `{ model, max_tokens, system, messages, tools, stream: true,
//!     thinking: {type:"adaptive"}, output_config: {effort} }`
//! - 사고/효과: 4.6+ 모델은 `thinking: {type:"adaptive"}` + `output_config.effort`
//!   (`budget_tokens` 는 4.7/4.8 에서 400). 이 어댑터의 기본 모델은 `claude-opus-4-8`
//!   (프로젝트 전체 기본 프로바이더는 OpenAI/ChatGPT 5.5 — 설정의 default_provider 참고).
//! - 스트림 이벤트 → 코어 이벤트 매핑:
//!     `content_block_delta`(text_delta)     → TextDelta
//!     `content_block_delta`(thinking_delta) → ThinkingDelta
//!     `content_block_start`(tool_use)       → ToolUseStart
//!     `content_block_delta`(input_json_delta) → ToolUseInputDelta
//!     `content_block_stop`                  → ContentBlockStop
//!     `message_delta`(stop_reason,usage) / `message_stop` → MessageStop

use async_trait::async_trait;

use scv_core::message::{Message, StreamEvent};
use scv_core::provider::{CompletionRequest, EventStream, ModelInfo, Provider, ToolSchema};
use scv_core::Result;

const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";

#[derive(Debug)]
pub struct AnthropicProvider {
    model: String,
    api_key: String,
    base_url: String,
    http: reqwest::Client,
    models: Vec<ModelInfo>,
}

impl AnthropicProvider {
    pub fn new(model: String, api_key: String, base_url: Option<String>) -> Self {
        Self {
            model,
            api_key,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            http: reqwest::Client::new(),
            models: vec![ModelInfo {
                id: "claude-opus-4-8".into(),
                context_window: 1_000_000,
                max_output_tokens: 128_000,
                supports_thinking: true,
            }],
        }
    }

    /// 코어 요청을 Anthropic 와이어 JSON 으로 변환한다.
    fn to_wire(&self, req: &CompletionRequest) -> serde_json::Value {
        let mut body = serde_json::json!({
            "model": req.model,
            "max_tokens": req.max_tokens,
            "stream": true,
            "thinking": { "type": "adaptive" },
            // messages/tools/system 변환은 별도 함수에서 채운다(블록 ↔ Anthropic content).
            "messages": [],
        });
        if let Some(system) = &req.system {
            body["system"] = serde_json::json!(system);
        }
        if let Some(effort) = req.effort {
            body["output_config"] = serde_json::json!({ "effort": format!("{effort:?}").to_lowercase() });
        }
        // TODO: req.messages → Anthropic content 블록, req.tools → tools[] 변환.
        body
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        "anthropic"
    }

    fn models(&self) -> &[ModelInfo] {
        &self.models
    }

    async fn stream(&self, request: CompletionRequest) -> Result<EventStream> {
        let _wire = self.to_wire(&request);
        let _ = (&self.api_key, &self.base_url, &self.http, &self.model, ANTHROPIC_VERSION);

        // 실제 구현 골격:
        //   let resp = self.http.post(format!("{}/v1/messages", self.base_url))
        //       .header("x-api-key", &self.api_key)
        //       .header("anthropic-version", ANTHROPIC_VERSION)
        //       .json(&wire).send().await?;
        //   let sse = resp.bytes_stream().eventsource();
        //   let mapped = sse.map(|ev| parse_anthropic_event(ev));  // → StreamEvent
        //   Ok(Box::pin(mapped))
        //
        // 스캐폴드 단계에서는 빈 스트림을 돌려준다.
        let empty = futures::stream::empty::<Result<StreamEvent>>();
        Ok(Box::pin(empty))
    }

    async fn count_tokens(
        &self,
        _system: Option<&str>,
        _messages: &[Message],
        _tools: &[ToolSchema],
    ) -> Result<u64> {
        // TODO: POST {base_url}/v1/messages/count_tokens
        //   헤더: x-api-key, anthropic-version → 응답의 input_tokens 반환.
        //   평상시 compaction 신호로는 returned usage(MessageStop)를 우선 사용한다.
        Ok(0)
    }
}
