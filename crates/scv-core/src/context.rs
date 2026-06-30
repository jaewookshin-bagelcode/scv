//! 컨텍스트 윈도 관리.
//!
//! 대화가 길어지면 토큰이 모델의 컨텍스트 윈도를 넘본다. [`ContextManager`] 는
//! 한도에 근접하면 오래된 부분을 **요약(compaction)** 하거나 잘라내 히스토리를
//! 줄인다. 전략은 교체 가능하도록 trait 으로 둔다.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;

use crate::message::{ContentBlock, Message, Role, StreamEvent};
use crate::provider::{CompletionRequest, EventStream, Provider, ThinkingMode};
use crate::Result;

/// 컨텍스트 관리 전략.
#[async_trait]
pub trait ContextManager: Send + Sync {
    /// 다음 요청을 만들기 전에 메시지 히스토리를 다듬는다.
    ///
    /// `last_input_tokens` 는 **직전 응답의 입력 토큰 수**(`StreamEvent::MessageStop` 의
    /// `Usage.input_tokens`, 첫 턴엔 0) — compaction 트리거의 주 신호다(추가 호출 0이라
    /// 가장 싸다, ARCHITECTURE §4.2). 반환값은 요청에 실제로 보낼 메시지 목록. 입력을 그대로
    /// 돌려주면 무동작.
    async fn prepare(&self, messages: Vec<Message>, last_input_tokens: u64)
        -> Result<Vec<Message>>;
}

/// 아무것도 하지 않는 기본 전략(초기 구현/테스트용).
#[derive(Debug, Default)]
pub struct NoopContextManager;

#[async_trait]
impl ContextManager for NoopContextManager {
    async fn prepare(
        &self,
        messages: Vec<Message>,
        _last_input_tokens: u64,
    ) -> Result<Vec<Message>> {
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
    async fn prepare(
        &self,
        messages: Vec<Message>,
        _last_input_tokens: u64,
    ) -> Result<Vec<Message>> {
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

/// 임계 초과 시 **오래된 앞부분을 LLM 으로 요약(compaction)** 하는 전략. 최근
/// `keep_recent` 개 메시지는 verbatim 으로 두어 정밀도를 보존하고, 그 이전은 한 통의 요약
/// 메시지로 접는다. 요약 호출은 주입된 [`Provider`] 로 한다(전략이 모델을 호출하는 첫 사례).
///
/// 트리거: `last_input_tokens > threshold_tokens`(직전 응답 usage 기반, ARCHITECTURE §4.2).
/// 임계 이하거나 접을 앞부분이 없으면 무동작.
///
/// NOTE: 요약 메시지는 `User` 역할로 앞에 둔다 — OpenAI/Ollama 는 연속 user 를 허용한다.
/// Anthropic(엄격한 역할 교대)는 4a 에서 어댑터가 흡수하거나 호출부가 keep_recent 를 턴
/// 경계에 맞춘다.
pub struct SummarizingContextManager {
    provider: Arc<dyn Provider>,
    model: String,
    threshold_tokens: u64,
    keep_recent: usize,
    max_summary_tokens: u32,
}

impl std::fmt::Debug for SummarizingContextManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SummarizingContextManager")
            .field("model", &self.model)
            .field("threshold_tokens", &self.threshold_tokens)
            .field("keep_recent", &self.keep_recent)
            .finish_non_exhaustive()
    }
}

impl SummarizingContextManager {
    pub fn new(
        provider: Arc<dyn Provider>,
        model: String,
        threshold_tokens: u64,
        keep_recent: usize,
    ) -> Self {
        Self {
            provider,
            model,
            threshold_tokens,
            keep_recent,
            max_summary_tokens: 1024,
        }
    }
}

const SUMMARY_SYSTEM: &str = "You compress a coding-assistant conversation into a dense summary. \
Preserve decisions made, facts learned, file paths, identifiers, and still-open tasks. \
Drop pleasantries and redundancy. Output only the summary.";

#[async_trait]
impl ContextManager for SummarizingContextManager {
    async fn prepare(
        &self,
        messages: Vec<Message>,
        last_input_tokens: u64,
    ) -> Result<Vec<Message>> {
        // 트리거: 임계 이하면 무동작. 접을 앞부분이 없어도 무동작.
        if last_input_tokens <= self.threshold_tokens {
            return Ok(messages);
        }
        let cutoff = messages.len().saturating_sub(self.keep_recent);
        if cutoff == 0 {
            return Ok(messages);
        }
        let (old, recent) = messages.split_at(cutoff);

        let transcript = render_transcript(old);
        let request = CompletionRequest {
            model: self.model.clone(),
            system: Some(SUMMARY_SYSTEM.to_string()),
            messages: vec![Message::user(format!(
                "Summarize the conversation so far:\n\n{transcript}"
            ))],
            tools: vec![],
            max_tokens: self.max_summary_tokens,
            effort: None,
            thinking: ThinkingMode::Disabled,
        };
        let summary = collect_stream_text(self.provider.stream(request).await?).await?;

        let mut out = Vec::with_capacity(1 + recent.len());
        out.push(Message::user(format!(
            "[earlier conversation summarized]\n{}",
            summary.trim()
        )));
        out.extend(recent.iter().cloned());
        Ok(out)
    }
}

/// 스트림에서 텍스트 증분만 모아 한 문자열로(요약 응답 수집용).
async fn collect_stream_text(mut stream: EventStream) -> Result<String> {
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        if let StreamEvent::TextDelta(t) = event? {
            text.push_str(&t);
        }
    }
    Ok(text)
}

/// 메시지들을 요약 입력용 평문 트랜스크립트로 펼친다(tool_result 는 길면 자른다).
fn render_transcript(messages: &[Message]) -> String {
    let mut s = String::new();
    for m in messages {
        let role = match m.role {
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::System => "System",
        };
        for block in &m.content {
            match block {
                ContentBlock::Text { text } => {
                    s.push_str(role);
                    s.push_str(": ");
                    s.push_str(text);
                    s.push('\n');
                }
                ContentBlock::ToolUse { name, .. } => {
                    s.push_str(&format!("{role} called tool `{name}`\n"));
                }
                ContentBlock::ToolResult { content, .. } => {
                    let trimmed: String = content.chars().take(200).collect();
                    s.push_str(&format!("[tool result] {trimmed}\n"));
                }
                ContentBlock::ServerToolUse { name, .. } => {
                    s.push_str(&format!("{role} used server tool `{name}`\n"));
                }
                ContentBlock::ServerToolResult { .. } => {
                    s.push_str("[server tool result]\n");
                }
                ContentBlock::Thinking { .. } => {} // 사고는 요약 입력에서 제외.
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Message, Usage};
    use crate::provider::{ModelInfo, ToolSchema};

    #[tokio::test]
    async fn noop_returns_input_unchanged() {
        let msgs = vec![Message::user("a"), Message::user("b")];
        let out = NoopContextManager.prepare(msgs, 0).await.unwrap();
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
            .prepare(messages, 0)
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

    /// 고정 요약 텍스트를 스트리밍하는 가짜 프로바이더(요약 호출을 가로챈다).
    struct FakeSummaryProvider;
    #[async_trait]
    impl Provider for FakeSummaryProvider {
        fn id(&self) -> &str {
            "fake"
        }
        fn models(&self) -> &[ModelInfo] {
            &[]
        }
        async fn stream(&self, _request: CompletionRequest) -> Result<EventStream> {
            let events = vec![
                Ok(StreamEvent::TextDelta("SUMMARY".into())),
                Ok(StreamEvent::MessageStop {
                    stop_reason: crate::message::StopReason::EndTurn,
                    usage: Usage::default(),
                }),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
        async fn count_tokens(
            &self,
            _s: Option<&str>,
            _m: &[Message],
            _t: &[ToolSchema],
        ) -> Result<u64> {
            Ok(0)
        }
    }

    fn summarizer(threshold: u64, keep: usize) -> SummarizingContextManager {
        SummarizingContextManager::new(Arc::new(FakeSummaryProvider), "m".into(), threshold, keep)
    }

    #[tokio::test]
    async fn summarizer_noop_below_threshold() {
        let msgs = vec![Message::user("a"), Message::user("b"), Message::user("c")];
        // last_input_tokens(10) <= threshold(100) → 무동작.
        let out = summarizer(100, 1).prepare(msgs, 10).await.unwrap();
        assert_eq!(out.len(), 3);
    }

    #[tokio::test]
    async fn summarizer_folds_old_prefix_when_over_threshold() {
        let msgs = vec![
            Message::user("oldest"),
            Message::assistant(vec![ContentBlock::text("old reply")]),
            Message::user("recent question"),
        ];
        // threshold 초과 + keep_recent=1 → 앞 2개를 요약 1통으로 접고 마지막 1개 보존.
        let out = summarizer(100, 1).prepare(msgs, 500).await.unwrap();
        assert_eq!(out.len(), 2, "summary + 1 recent");
        match &out[0].content[0] {
            ContentBlock::Text { text } => {
                assert!(text.contains("summarized"), "got: {text}");
                assert!(
                    text.contains("SUMMARY"),
                    "fake summary text folded in: {text}"
                );
            }
            other => panic!("expected text summary, got {other:?}"),
        }
        // 최근 메시지는 verbatim.
        match &out[1].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "recent question"),
            other => panic!("expected recent verbatim, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn summarizer_noop_when_nothing_old_to_fold() {
        // keep_recent >= len → cutoff 0 → 접을 앞부분이 없어 무동작(임계 초과여도).
        let msgs = vec![Message::user("only")];
        let out = summarizer(100, 5).prepare(msgs, 999).await.unwrap();
        assert_eq!(out.len(), 1);
    }

    #[tokio::test]
    async fn summarizer_renders_all_block_kinds_in_prefix() {
        use crate::message::Role;
        // 접히는 앞부분에 모든 블록 종류 + System 역할을 넣어 render_transcript 의 분기를 모두 탄다.
        let msgs = vec![
            Message {
                role: Role::System,
                content: vec![ContentBlock::text("sys note")],
            },
            Message::assistant(vec![
                ContentBlock::Thinking {
                    text: "hmm".into(),
                    signature: None,
                },
                ContentBlock::ToolUse {
                    id: "c1".into(),
                    name: "grep".into(),
                    input: serde_json::json!({}),
                },
            ]),
            Message {
                role: Role::User,
                content: vec![ContentBlock::ToolResult {
                    tool_use_id: "c1".into(),
                    content: "x".repeat(300), // 200자 초과 → 잘림 경로.
                    is_error: false,
                }],
            },
            Message::user("recent"),
        ];
        let out = summarizer(100, 1).prepare(msgs, 500).await.unwrap();
        assert_eq!(out.len(), 2, "summary + 1 recent");
    }

    #[test]
    fn summarizing_manager_debug_shows_config() {
        let s = format!("{:?}", summarizer(100, 4));
        assert!(s.contains("SummarizingContextManager"));
        assert!(s.contains("threshold_tokens"));
    }

    #[tokio::test]
    async fn fake_summary_provider_exposes_surface() {
        assert_eq!(FakeSummaryProvider.id(), "fake");
        assert!(FakeSummaryProvider.models().is_empty());
        assert_eq!(
            FakeSummaryProvider
                .count_tokens(None, &[], &[])
                .await
                .unwrap(),
            0
        );
    }
}
