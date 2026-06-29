//! 프로바이더 중립(provider-neutral) 대화 모델.
//!
//! Anthropic Messages API, OpenAI Chat Completions 등 프로바이더마다 와이어 포맷이
//! 다르다. 코어는 **하나의 내부 표현**만 다루고, 각 어댑터가 이 타입 ↔ 자기 와이어
//! 포맷을 변환한다. 덕분에 에이전트 루프/세션/TUI 는 프로바이더를 모른다.

use serde::{Deserialize, Serialize};

/// 메시지 발화 주체.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    /// 운영자 채널(시스템 지시). 일반적으로 대화 선두에만 둔다.
    System,
}

/// 메시지를 이루는 콘텐츠 블록. 한 메시지는 여러 블록을 가질 수 있다
/// (예: assistant 가 text + 여러 tool_use 블록을 동시에 낼 수 있음).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// 일반 텍스트.
    Text { text: String },

    /// 모델의 사고(thinking) 블록. 같은 모델로 대화를 이어갈 때는 그대로 되돌려준다.
    Thinking {
        text: String,
        /// 일부 프로바이더가 부여하는 서명. 보존 필요. 없으면 None.
        #[serde(skip_serializing_if = "Option::is_none")]
        signature: Option<String>,
    },

    /// 모델이 도구 호출을 요청한 블록.
    ToolUse {
        id: String,
        name: String,
        /// 도구 입력(JSON). 스키마는 도구가 정의한다.
        input: serde_json::Value,
    },

    /// 도구 실행 결과(다음 user 턴에 실어 보냄).
    ToolResult {
        tool_use_id: String,
        content: String,
        /// 실패 결과면 true. 모델이 복구를 시도할 수 있게 한다.
        #[serde(default)]
        is_error: bool,
    },
}

impl ContentBlock {
    /// 텍스트 블록 헬퍼.
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text { text: s.into() }
    }
}

/// 대화 한 턴(또는 한 메시지).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentBlock>,
}

impl Message {
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: vec![ContentBlock::text(text)],
        }
    }

    pub fn assistant(content: Vec<ContentBlock>) -> Self {
        Self {
            role: Role::Assistant,
            content,
        }
    }

    /// 이 메시지에 포함된 tool_use 블록들을 순회한다.
    pub fn tool_uses(&self) -> impl Iterator<Item = (&str, &str, &serde_json::Value)> {
        self.content.iter().filter_map(|b| match b {
            ContentBlock::ToolUse { id, name, input } => Some((id.as_str(), name.as_str(), input)),
            _ => None,
        })
    }
}

/// 모델이 응답을 멈춘 이유(프로바이더 공통으로 정규화).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// 자연스럽게 응답을 마침.
    EndTurn,
    /// max_tokens 한계에 도달(출력 잘림).
    MaxTokens,
    /// 도구 호출을 요청함 → 실행 후 결과를 돌려줘야 함.
    ToolUse,
    /// 안전상의 이유로 거부.
    Refusal,
    /// 사용자 지정 stop sequence 도달.
    StopSequence,
}

/// 토큰 사용량(비용/관측성).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
}

/// 스트리밍 응답을 프로바이더 공통으로 정규화한 이벤트.
///
/// 각 어댑터는 자기 SSE 포맷(Anthropic: `content_block_delta` 등)을 이 이벤트로 번역한다.
/// TUI 는 이 이벤트만 보고 화면을 그린다.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum StreamEvent {
    /// 응답 시작. 실제로 응답한 모델 id.
    MessageStart { model: String },
    /// 텍스트 증분.
    TextDelta(String),
    /// thinking 증분.
    ThinkingDelta(String),
    /// tool_use 블록 시작.
    ToolUseStart { id: String, name: String },
    /// tool_use 입력 JSON 의 증분(부분 문자열).
    ToolUseInputDelta { id: String, partial_json: String },
    /// 콘텐츠 블록 하나 종료.
    ContentBlockStop,
    /// 응답 종료. 정규화된 stop_reason 과 누적 usage.
    MessageStop {
        stop_reason: StopReason,
        usage: Usage,
    },
}

/// 에이전트 루프가 [`Observer`](crate::agent::Observer) 에 흘리는 **관찰 전용** 라이프
/// 사이클 통지(TUI 렌더·진행 표시용). 프로바이더 스트림 증분은 [`Self::Stream`] 으로
/// 감싸고, 도구 실행·권한·취소처럼 루프 차원의 사건은 별도 변형으로 알린다.
///
/// `on_event` 은 `()` 를 돌려줘 **되먹임이 불가**하다 — 인터럽트의 실제 메커니즘은
/// `CancellationToken`, 권한 승인은 `PermissionGate::decide` 의 반환값이다(ARCHITECTURE §4.5).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AgentEvent {
    /// 프로바이더 스트림 증분(텍스트/사고/도구 입력 등).
    Stream(StreamEvent),
    /// 도구 실행 시작.
    ToolStart { name: String },
    /// 도구 실행 종료(에러 여부와 모델에게 전달된 출력 포함).
    ToolEnd {
        name: String,
        content: String,
        is_error: bool,
    },
    /// `Ask` 도구가 권한 게이트의 결정을 기다린다(사후 통지).
    PermissionAsked { name: String },
    /// 사용자 인터럽트로 턴이 중단됐다(사후 통지).
    Interrupted,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_has_single_text_block() {
        let m = Message::user("hi");
        assert!(matches!(m.role, Role::User));
        assert_eq!(m.content.len(), 1);
        assert!(matches!(&m.content[0], ContentBlock::Text { text } if text == "hi"));
    }

    #[test]
    fn tool_uses_filters_only_tool_use_blocks() {
        let m = Message::assistant(vec![
            ContentBlock::text("thinking out loud"),
            ContentBlock::ToolUse {
                id: "c1".into(),
                name: "read".into(),
                input: serde_json::json!({ "path": "a" }),
            },
            ContentBlock::ToolUse {
                id: "c2".into(),
                name: "glob".into(),
                input: serde_json::json!({}),
            },
        ]);
        let uses: Vec<_> = m.tool_uses().collect();
        assert_eq!(uses.len(), 2);
        assert_eq!(uses[0].0, "c1");
        assert_eq!(uses[0].1, "read");
        assert_eq!(uses[1].1, "glob");
    }

    #[test]
    fn content_block_serde_uses_type_tag() {
        let block = ContentBlock::ToolResult {
            tool_use_id: "c1".into(),
            content: "ok".into(),
            is_error: false,
        };
        let json = serde_json::to_string(&block).unwrap();
        assert!(json.contains("\"type\":\"tool_result\""));
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            back,
            ContentBlock::ToolResult {
                is_error: false,
                ..
            }
        ));
    }

    #[test]
    fn stop_reason_serializes_snake_case() {
        assert_eq!(
            serde_json::to_string(&StopReason::ToolUse).unwrap(),
            "\"tool_use\""
        );
        assert_eq!(
            serde_json::to_string(&StopReason::EndTurn).unwrap(),
            "\"end_turn\""
        );
    }
}
