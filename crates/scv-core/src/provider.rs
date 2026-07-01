//! LLM 프로바이더 추상.
//!
//! 멀티 프로바이더 설계의 핵심. 코어/루프는 [`Provider`] trait 만 알고, 구체 어댑터
//! (Anthropic, OpenAI ...)는 `scv-providers` 크레이트에서 이 trait 을 구현한다.

use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};

use crate::message::{Message, StreamEvent};
use crate::Result;

/// 사고 강도. 프로바이더별 파라미터(effort 등)로 매핑된다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

/// 사고(thinking) 모드.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ThinkingMode {
    /// 모델이 알아서 사고 깊이를 정함(권장).
    #[default]
    Adaptive,
    /// 사고 비활성화.
    Disabled,
}

/// 프로바이더에 전달할 도구 스키마(중립 표현).
/// 어댑터가 이걸 자기 와이어 포맷의 tool 정의로 변환한다.
#[derive(Debug, Clone, Serialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema(Draft 2020-12 호환 부분집합).
    pub input_schema: serde_json::Value,
}

/// 한 번의 completion 요청(프로바이더 중립).
#[derive(Debug, Clone)]
pub struct CompletionRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
    pub max_tokens: u32,
    pub effort: Option<Effort>,
    pub thinking: ThinkingMode,
}

/// 정규화된 스트리밍 이벤트의 비동기 스트림.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;

/// 모델 메타데이터(능력 조회/검증용).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub context_window: u64,
    pub max_output_tokens: u32,
    pub supports_thinking: bool,
}

/// LLM 프로바이더.
///
/// 스트리밍을 1급으로 둔다. (긴 출력/대형 max_tokens 에서 HTTP 타임아웃을 피하려면
/// 스트리밍이 사실상 필수이고, TUI 의 실시간 출력에도 필요하다.)
#[async_trait]
pub trait Provider: Send + Sync {
    /// 설정에서 참조하는 프로바이더 id (예: "anthropic").
    fn id(&self) -> &str;

    /// 이 프로바이더가 지원하는 모델 목록(정적 폴백). 오프라인·직결에서도 동작한다.
    fn models(&self) -> &[ModelInfo];

    /// 실시간 모델 카탈로그. 기본은 정적 [`Self::models`] 를 복제해 돌려준다. aiproxy 처럼
    /// 카탈로그 API 가 있는 어댑터는 이를 오버라이드해 실제 제공 모델을 가져오고, 조회 실패
    /// 시엔 정적 목록으로 폴백한다. `/models` 표시와 `/model` 검증이 이 결과를 쓴다.
    async fn list_models(&self) -> Result<Vec<ModelInfo>> {
        Ok(self.models().to_vec())
    }

    /// 요청을 보내고 정규화된 이벤트 스트림을 받는다.
    async fn stream(&self, request: CompletionRequest) -> Result<EventStream>;

    /// 주어진 요청 구성의 입력 토큰 수를 센다(compaction 트리거 판단·사전 점검용).
    ///
    /// 어댑터별 구현(프로바이더 지식은 어댑터에 머문다):
    /// - Anthropic: `POST /v1/messages/count_tokens` 로 서버에서 정확히 계산
    /// - OpenAI: 로컬 토크나이저(tiktoken `o200k_base` 류)로 추정(count 엔드포인트 없음)
    ///
    /// NOTE: **평상시 compaction 신호는 직전 응답의 [`crate::message::Usage`]`.input_tokens`**
    /// ([`crate::message::StreamEvent::MessageStop`])를 쓴다 — 추가 호출이 0이라 더 싸다.
    /// 이 메서드는 첫 전송 전 거대 입력 사전 점검, 또는 정밀 절삭량 계산용 보조 경로다.
    async fn count_tokens(
        &self,
        system: Option<&str>,
        messages: &[Message],
        tools: &[ToolSchema],
    ) -> Result<u64>;
}
