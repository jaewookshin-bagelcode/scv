//! 컨텍스트 윈도 관리.
//!
//! 대화가 길어지면 토큰이 모델의 컨텍스트 윈도를 넘본다. [`ContextManager`] 는
//! 한도에 근접하면 오래된 부분을 **요약(compaction)** 하거나 잘라내 히스토리를
//! 줄인다. 전략은 교체 가능하도록 trait 으로 둔다.

use async_trait::async_trait;

use crate::message::{ContentBlock, Message};
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

/// 오래된 `tool_result` 블록의 content 를 **요약하지 않고 비워**(placeholder 로 치환)
/// 컨텍스트를 줄이는 전략(Anthropic context editing 과 같은 개념, ARCHITECTURE §4.2).
/// 끝에서 `keep_recent` 개 메시지의 결과는 그대로 두고, 그 이전 tool_result 만 비운다.
///
/// **무손실**: 원본(읽은 파일·검색 결과)은 디스크와 세션 JSONL 에 남아 있어, 모델이
/// 다시 필요하면 `read`/`grep` 으로 정밀 재조회한다. LLM 호출 0(요약 방식과 달리).
#[derive(Debug, Clone)]
pub struct ClearToolResultsManager {
    /// 끝에서부터 이 개수만큼의 메시지는 `tool_result` 를 비우지 않는다.
    pub keep_recent: usize,
}

impl ClearToolResultsManager {
    pub fn new(keep_recent: usize) -> Self {
        Self { keep_recent }
    }
}

#[async_trait]
impl ContextManager for ClearToolResultsManager {
    async fn prepare(&self, messages: Vec<Message>) -> Result<Vec<Message>> {
        let cutoff = messages.len().saturating_sub(self.keep_recent);
        let cleared = messages
            .into_iter()
            .enumerate()
            .map(|(i, mut msg)| {
                if i < cutoff {
                    for block in &mut msg.content {
                        if let ContentBlock::ToolResult { content, .. } = block {
                            if !content.is_empty() {
                                *content = format!(
                                    "[cleared {} bytes — re-read the source if needed]",
                                    content.len()
                                );
                            }
                        }
                    }
                }
                msg
            })
            .collect();
        Ok(cleared)
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
//      editing). 원본이 필요하면 디스크(세션 JSONL/파일)에서 도구로 재조회한다. ✅ 구현됨.
// TODO(compaction): SummarizingContextManager(LLM 요약) 구현 — Provider 호출이 필요해 별도.
//   또한 임계(`compact_threshold_tokens`) 기반 트리거를 루프에 배선한다(현재 둘 다 미배선).

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;

    #[tokio::test]
    async fn noop_returns_input_unchanged() {
        let msgs = vec![Message::user("a"), Message::user("b")];
        let out = NoopContextManager.prepare(msgs).await.unwrap();
        assert_eq!(out.len(), 2);
    }

    #[tokio::test]
    async fn clears_old_tool_results_but_keeps_recent() {
        use crate::message::Role;
        let tool_msg = |c: &str| Message {
            role: Role::User,
            content: vec![ContentBlock::ToolResult {
                tool_use_id: "t".into(),
                content: c.into(),
                is_error: false,
            }],
        };
        let messages = vec![
            tool_msg("OLD big output"),
            Message::user("mid"),
            tool_msg("RECENT"),
        ];

        // keep_recent=1 → 마지막 메시지만 보존, 그 이전 tool_result 는 비운다.
        let out = ClearToolResultsManager::new(1)
            .prepare(messages)
            .await
            .unwrap();

        match &out[0].content[0] {
            ContentBlock::ToolResult { content, .. } => {
                assert!(content.starts_with("[cleared"), "got: {content}");
            }
            other => panic!("expected tool_result, got {other:?}"),
        }
        match &out[2].content[0] {
            ContentBlock::ToolResult { content, .. } => assert_eq!(content, "RECENT"),
            other => panic!("expected tool_result, got {other:?}"),
        }
    }
}
