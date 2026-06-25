//! 컨텍스트 윈도 관리.
//!
//! 대화가 길어지면 토큰이 모델의 컨텍스트 윈도를 넘본다. [`ContextManager`] 는
//! 한도에 근접하면 오래된 부분을 **요약(compaction)** 하거나 잘라내 히스토리를
//! 줄인다. 전략은 교체 가능하도록 trait 으로 둔다.

use async_trait::async_trait;

use crate::message::Message;
use crate::Result;

/// 컨텍스트 관리 전략.
#[async_trait]
pub trait ContextManager: Send + Sync {
    /// 다음 요청을 만들기 전에 메시지 히스토리를 다듬는다.
    ///
    /// 반환값은 요청에 실제로 보낼 메시지 목록. 입력을 그대로 돌려주면 무동작.
    async fn prepare(&self, messages: Vec<Message>) -> Result<Vec<Message>>;
}

/// 아무것도 하지 않는 기본 전략(초기 구현/테스트용).
#[derive(Debug, Default)]
pub struct NoopContextManager;

#[async_trait]
impl ContextManager for NoopContextManager {
    async fn prepare(&self, messages: Vec<Message>) -> Result<Vec<Message>> {
        Ok(messages)
    }
}

// 결정된 설계(로드맵 §8):
//
// 트리거 신호 — **직전 응답의 `Usage.input_tokens`(StreamEvent::MessageStop)를 우선**
//   사용한다. 추가 호출이 0이라 가장 싸다. 첫 전송 전 거대 입력 등 사전 점검이 필요할
//   때만 `Provider::count_tokens`(어댑터별: Anthropic count 엔드포인트 / OpenAI tiktoken)
//   를 보조로 쓴다. 임계치는 설정 `[session].compact_threshold_tokens`(기본 150_000).
//
// 두 가지 전략을 ContextManager 구현으로 제공할 수 있다:
//   1) `SummarizingContextManager` — 임계 초과 시 오래된 앞부분을 LLM 으로 요약(compaction).
//      최근 턴은 verbatim 유지해 정밀도 보존. 요약 호출도 Provider 를 통해 한다.
//   2) `ClearToolResultsManager` — 오래된 tool_result 블록을 *요약 말고 비운다*(context
//      editing). 원본이 필요하면 디스크(세션 JSONL/파일)에서 도구로 재조회한다.
// TODO(compaction): 위 두 매니저를 구현한다.
