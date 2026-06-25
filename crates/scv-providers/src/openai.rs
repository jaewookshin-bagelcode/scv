//! OpenAI Chat Completions 어댑터(멀티 프로바이더 검증용).
//!
//! Anthropic 과 와이어 포맷이 다르지만(content 가 문자열, tool_calls 구조 등) 코어가
//! 보는 인터페이스는 동일하다. 이 어댑터가 그 차이를 흡수한다 → 멀티 프로바이더 추상이
//! 실제로 성립함을 보여주는 두 번째 구현.
//!
//! - 엔드포인트: `POST {base_url}/chat/completions`
//! - 헤더: `Authorization: Bearer {api_key}`
//! - 매핑: 코어 ContentBlock ↔ OpenAI message/tool_calls, SSE delta ↔ StreamEvent

use async_trait::async_trait;

use scv_core::message::{Message, StreamEvent};
use scv_core::provider::{CompletionRequest, EventStream, ModelInfo, Provider, ToolSchema};
use scv_core::Result;

const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";

#[derive(Debug)]
pub struct OpenAiProvider {
    model: String,
    api_key: String,
    base_url: String,
    http: reqwest::Client,
    models: Vec<ModelInfo>,
}

impl OpenAiProvider {
    pub fn new(model: String, api_key: String, base_url: Option<String>) -> Self {
        Self {
            model,
            api_key,
            base_url: base_url.unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            http: reqwest::Client::new(),
            // 기본 모델: ChatGPT 5.5.
            models: vec![ModelInfo {
                id: "gpt-5.5".into(),
                context_window: 400_000,
                max_output_tokens: 128_000,
                supports_thinking: true,
            }],
        }
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> &str {
        "openai"
    }

    fn models(&self) -> &[ModelInfo] {
        &self.models
    }

    async fn stream(&self, _request: CompletionRequest) -> Result<EventStream> {
        let _ = (&self.api_key, &self.base_url, &self.http, &self.model);
        // TODO: Chat Completions 요청 구성 + SSE delta → StreamEvent 매핑.
        let empty = futures::stream::empty::<Result<StreamEvent>>();
        Ok(Box::pin(empty))
    }

    async fn count_tokens(
        &self,
        _system: Option<&str>,
        _messages: &[Message],
        _tools: &[ToolSchema],
    ) -> Result<u64> {
        // TODO: 로컬 토크나이저(tiktoken o200k_base)로 추정. OpenAI 엔 count 엔드포인트 없음.
        //   평상시 compaction 신호로는 returned usage 를 우선 사용한다.
        Ok(0)
    }
}
