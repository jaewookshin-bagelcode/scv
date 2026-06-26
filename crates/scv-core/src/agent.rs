//! Agentic loop — 한 사용자 턴을 끝까지 구동하는 엔진.
//!
//! 흐름:
//! ```text
//!  user 입력
//!     │
//!     ▼
//!  [컨텍스트 준비] ─► [Provider.stream] ─► (스트림 이벤트를 Observer 로 흘리며 assistant 메시지로 집계)
//!     ▲                                          │
//!     │                              stop_reason == tool_use ?
//!     │                                ├── no ──► 턴 종료
//!     │                                └── yes ─► [권한 게이트] ─► [도구 실행(병렬 가능)] ─┐
//!     └──────────────────  tool_result 들을 user 메시지로 push  ◄────────────────────────┘
//! ```
//!
//! 이 엔진은 [`Provider`]/[`Tool`]/[`PermissionGate`] 의 **구체 타입을 모른다** —
//! 전부 trait object 로 받는다. 그래서 어떤 프로바이더·도구 조합과도 동작한다.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;

use crate::context::ContextManager;
use crate::message::{AgentEvent, ContentBlock, Message, Role, StopReason, StreamEvent};
use crate::provider::{CompletionRequest, Effort, Provider, ThinkingMode};
use crate::session::Session;
use crate::tool::{PermissionGate, PermissionLevel, Tool, ToolContext, ToolRegistry};
use crate::{Error, Result};

/// 루프의 라이프사이클 통지([`AgentEvent`])를 받아 화면 출력 등에 쓰는 관찰자.
/// **관찰 전용**(`()` 반환 → 되먹임 불가, ARCHITECTURE §4.5). TUI 가 구현한다.
#[async_trait]
pub trait Observer: Send + Sync {
    async fn on_event(&self, event: &AgentEvent);
}

/// 아무것도 하지 않는 관찰자(비대화형/테스트용).
#[derive(Debug, Default)]
pub struct NullObserver;

#[async_trait]
impl Observer for NullObserver {
    async fn on_event(&self, _event: &AgentEvent) {}
}

/// 에이전트 루프 실행에 필요한 구성요소 묶음.
pub struct Agent {
    pub provider: Arc<dyn Provider>,
    pub tools: ToolRegistry,
    pub permissions: Arc<dyn PermissionGate>,
    pub context: Arc<dyn ContextManager>,
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub effort: Option<Effort>,
    pub max_tool_iterations: usize,
    pub tool_ctx: ToolContext,
}

// trait object 필드(Provider/PermissionGate/ContextManager)는 Debug 가 아니므로
// 수동으로 스칼라 필드만 출력한다.
impl std::fmt::Debug for Agent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Agent")
            .field("provider", &self.provider.id())
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("effort", &self.effort)
            .field("max_tool_iterations", &self.max_tool_iterations)
            .field("tools", &self.tools)
            .finish_non_exhaustive()
    }
}

impl Agent {
    /// 사용자 입력 한 건을 처리하고, 턴이 끝나면 세션을 갱신한다.
    ///
    /// `observer` 로 스트림 이벤트가 실시간 전달된다(TUI 출력).
    pub async fn run_turn(
        &self,
        session: &mut Session,
        user_input: String,
        observer: &dyn Observer,
    ) -> Result<()> {
        session.push(Message::user(user_input));
        let cancel = &self.tool_ctx.cancel;
        // compaction 트리거 신호: 직전 응답의 입력 토큰 수(MessageStop usage). 첫 턴은 0.
        let mut last_input_tokens: u64 = 0;

        for iteration in 0..self.max_tool_iterations {
            // 협조적 취소 ①: 이터레이션 진입부 체크포인트.
            if cancel.is_cancelled() {
                observer.on_event(&AgentEvent::Interrupted).await;
                return Err(Error::Cancelled);
            }

            // 1. 컨텍스트 준비(임계 초과 시 compaction — 직전 입력 토큰을 신호로 넘긴다).
            let messages = self
                .context
                .prepare(session.messages.clone(), last_input_tokens)
                .await?;

            // 2. 요청 구성 → 스트리밍 호출.
            let request = CompletionRequest {
                model: self.model.clone(),
                system: Some(self.system_prompt.clone()),
                messages,
                tools: self.tools.schemas(),
                max_tokens: self.max_tokens,
                effort: self.effort,
                thinking: ThinkingMode::Adaptive,
            };
            let mut stream = self.provider.stream(request).await?;

            // 3. 스트림을 집계해 assistant 메시지 한 통을 만든다. 협조적 취소 ②:
            //    스트림 소비를 취소 신호와 select! 로 경쟁시켜 Ctrl-C 시 즉시 멈춘다.
            let mut assembler = MessageAssembler::default();
            let mut stop_reason = StopReason::EndTurn;
            let interrupted = loop {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => break true,
                    next = stream.next() => match next {
                        None => break false,
                        Some(event) => {
                            let event = event?;
                            if let StreamEvent::MessageStop { stop_reason: sr, usage } = &event {
                                stop_reason = *sr;
                                // 다음 이터레이션의 compaction 트리거 신호로 쓴다.
                                last_input_tokens = usage.input_tokens;
                            }
                            observer.on_event(&AgentEvent::Stream(event.clone())).await;
                            assembler.apply(event);
                        }
                    }
                }
            };

            // 중단 시에도 모은 부분 텍스트를 세션에 보존한다(빈 메시지는 넣지 않음).
            let assistant = Message::assistant(assembler.finish());
            if !assistant.content.is_empty() {
                session.push(assistant.clone());
            }
            if interrupted {
                observer.on_event(&AgentEvent::Interrupted).await;
                return Err(Error::Cancelled);
            }

            // 4. 더 이상 도구 호출이 없으면 턴 종료.
            if stop_reason != StopReason::ToolUse {
                return Ok(());
            }

            // 방어: stop_reason 이 ToolUse 인데 실제 tool_use 블록이 없으면(모델/어댑터 이상)
            // 빈 tool_result 를 무한히 되돌리며 MaxIterations 까지 도는 대신 턴을 종료한다.
            if assistant.tool_uses().next().is_none() {
                tracing::warn!("stop_reason=ToolUse 인데 tool_use 블록이 없어 턴을 종료한다");
                return Ok(());
            }

            // 협조적 취소 ③: 도구 실행 직전 체크포인트.
            if cancel.is_cancelled() {
                observer.on_event(&AgentEvent::Interrupted).await;
                return Err(Error::Cancelled);
            }

            // 5. 도구 호출 처리 → tool_result 들을 하나의 user 메시지로 모은다.
            tracing::debug!(iteration, "executing tool calls");
            let results = self.execute_tool_calls(&assistant, observer).await?;
            session.push(Message {
                role: Role::User,
                content: results,
            });
        }

        Err(Error::MaxIterations(self.max_tool_iterations))
    }

    /// assistant 메시지의 모든 tool_use 블록을 실행해 tool_result 블록들을 만든다.
    ///
    /// **권한은 순차로** 해소한다(대화형 `Ask` 프롬프트는 한 번에 하나, 거부 시 턴 중단).
    /// 그 다음 **`parallel_safe` 도구(read/glob/grep 등 부작용 없음)는 `join_all` 로 동시
    /// 실행**하고, 비-parallel 도구(write/edit/bash)는 순차로 실행한다. tool_result 는
    /// tool_use_id 로 매칭되지만, 결정성을 위해 **원래 tool_use 순서**로 모은다.
    async fn execute_tool_calls(
        &self,
        assistant: &Message,
        observer: &dyn Observer,
    ) -> Result<Vec<ContentBlock>> {
        let uses: Vec<(String, String, serde_json::Value)> = assistant
            .tool_uses()
            .map(|(id, name, input)| (id.to_string(), name.to_string(), input.clone()))
            .collect();

        // 1단계: 권한을 순차 해소해 실행 계획을 만든다.
        let mut plans: Vec<ToolPlan> = Vec::with_capacity(uses.len());
        for (_id, name, input) in &uses {
            // 협조적 취소: 권한 해소 사이 체크포인트.
            if self.tool_ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let Some(tool) = self.tools.get(name) else {
                plans.push(ToolPlan::Unknown);
                continue;
            };
            // 권한 결정. 도구가 선언한 기준 권한을 게이트가 최종 확정한다.
            //  - Allow(읽기 전용 등 부작용 없음) → 게이트에 묻지 않고 허용.
            //  - Deny(예: workdir 밖 경로) → 즉시 거부.
            //  - Ask(되돌리기 어려운 동작) → 게이트(정적 정책 + 대화형)가 최종 결정.
            let level = match tool.permission(input) {
                PermissionLevel::Allow => PermissionLevel::Allow,
                PermissionLevel::Deny => PermissionLevel::Deny,
                PermissionLevel::Ask => {
                    observer
                        .on_event(&AgentEvent::PermissionAsked { name: name.clone() })
                        .await;
                    self.permissions.decide(name, input).await
                }
            };
            // fail-closed: 명시적 Allow 만 실행한다. 게이트가 끝내 Ask 를 돌려주면
            // (대화형 동의를 못 받은 상태) 실행하지 않고 거부한다 — write/bash 같은 Ask
            // 도구가 모달 없이 무단 실행되는 것을 막는다.
            if level != PermissionLevel::Allow {
                return Err(Error::PermissionDenied(name.clone()));
            }
            let parallel = tool.parallel_safe();
            plans.push(ToolPlan::Run {
                tool: tool.clone(),
                parallel,
            });
        }

        // 2단계: 실행. 결과는 인덱스로 채워 원래 순서를 보존한다.
        let mut results: Vec<Option<ContentBlock>> = (0..uses.len()).map(|_| None).collect();

        // 2a. parallel_safe 그룹을 동시에 실행(join_all). 부작용이 없어 순서 무관·경쟁 안전.
        if self.tool_ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let parallel_futures: Vec<_> = plans
            .iter()
            .enumerate()
            .filter_map(|(i, plan)| match plan {
                ToolPlan::Run {
                    tool,
                    parallel: true,
                } => {
                    let tool = tool.clone();
                    let (id, name, input) = uses[i].clone();
                    Some(async move {
                        observer
                            .on_event(&AgentEvent::ToolStart { name: name.clone() })
                            .await;
                        let output = tool.invoke(input, &self.tool_ctx).await;
                        observer
                            .on_event(&AgentEvent::ToolEnd {
                                name,
                                is_error: output.is_error,
                            })
                            .await;
                        (
                            i,
                            ContentBlock::ToolResult {
                                tool_use_id: id,
                                content: output.content,
                                is_error: output.is_error,
                            },
                        )
                    })
                }
                _ => None,
            })
            .collect();
        for (i, block) in futures::future::join_all(parallel_futures).await {
            results[i] = Some(block);
        }

        // 2b. 나머지(비-parallel 실행 + unknown 도구)는 순차로.
        for (i, plan) in plans.iter().enumerate() {
            if results[i].is_some() {
                continue;
            }
            let (id, name, input) = &uses[i];
            let block = match plan {
                ToolPlan::Unknown => ContentBlock::ToolResult {
                    tool_use_id: id.clone(),
                    content: format!("unknown tool: {name}"),
                    is_error: true,
                },
                ToolPlan::Run { tool, .. } => {
                    // 협조적 취소: 비가역 도구 실행 사이 체크포인트.
                    if self.tool_ctx.cancel.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    observer
                        .on_event(&AgentEvent::ToolStart { name: name.clone() })
                        .await;
                    let output = tool.invoke(input.clone(), &self.tool_ctx).await;
                    observer
                        .on_event(&AgentEvent::ToolEnd {
                            name: name.clone(),
                            is_error: output.is_error,
                        })
                        .await;
                    ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: output.content,
                        is_error: output.is_error,
                    }
                }
            };
            results[i] = Some(block);
        }

        Ok(results.into_iter().map(|b| b.expect("filled")).collect())
    }
}

/// 한 tool_use 의 실행 계획(권한 해소 후). `execute_tool_calls` 의 1→2단계 사이 매개.
enum ToolPlan {
    /// 알 수 없는 도구 → tool_result 에 에러로 돌려준다(모델이 복구 시도 가능).
    Unknown,
    /// 실행 허가됨. `parallel` 이면 동시 실행 그룹에 들어간다.
    Run { tool: Arc<dyn Tool>, parallel: bool },
}

/// 스트림 이벤트를 받아 하나의 assistant 메시지(콘텐츠 블록 리스트)로 누적한다.
///
/// **순수 변환**(부작용 없음): SSE 바이트 수신은 [`Provider`] 어댑터가, 이벤트 → 메시지
/// 집계는 여기서 한다 — functional core / imperative shell(CODING_RULES §4.1).
///
/// 블록 경계: 새 블록이 시작되면(예: 텍스트 → tool_use) 직전 블록을 **자동으로 닫는다**.
/// 따라서 어댑터가 [`StreamEvent::ContentBlockStop`] 을 매 블록마다 보내지 않아도 순서가
/// 보존된다(OpenAI 는 명시적 블록 종료 이벤트가 없다 — Anthropic 만 보낸다). 마지막으로
/// 열린 블록은 [`Self::finish`] 가 닫는다.
#[derive(Debug, Default)]
struct MessageAssembler {
    blocks: Vec<ContentBlock>,
    /// 아직 증분 누적 중인(닫히지 않은) 블록.
    open: Option<OpenBlock>,
}

/// 누적 중인 콘텐츠 블록의 가변 버퍼. 닫힐 때 [`ContentBlock`] 으로 확정된다.
#[derive(Debug)]
enum OpenBlock {
    Text(String),
    Thinking {
        text: String,
        signature: Option<String>,
    },
    ToolUse {
        id: String,
        name: String,
        json: String,
    },
}

impl MessageAssembler {
    fn apply(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::TextDelta(t) => match &mut self.open {
                Some(OpenBlock::Text(buf)) => buf.push_str(&t),
                _ => {
                    self.close_open();
                    self.open = Some(OpenBlock::Text(t));
                }
            },
            StreamEvent::ThinkingDelta(t) => match &mut self.open {
                Some(OpenBlock::Thinking { text, .. }) => text.push_str(&t),
                _ => {
                    self.close_open();
                    self.open = Some(OpenBlock::Thinking {
                        text: t,
                        signature: None,
                    });
                }
            },
            StreamEvent::ToolUseStart { id, name } => {
                self.close_open();
                self.open = Some(OpenBlock::ToolUse {
                    id,
                    name,
                    json: String::new(),
                });
            }
            StreamEvent::ToolUseInputDelta { partial_json, .. } => {
                if let Some(OpenBlock::ToolUse { json, .. }) = &mut self.open {
                    json.push_str(&partial_json);
                }
                // open 이 tool_use 가 아니면 무시한다(정상 흐름에선 발생하지 않음).
            }
            StreamEvent::ContentBlockStop => self.close_open(),
            // MessageStart/MessageStop 은 루프가 직접 처리한다(여기선 무시).
            _ => {}
        }
    }

    /// 현재 열린 블록을 완성된 [`ContentBlock`] 으로 확정해 push 한다(빈 텍스트는 버림).
    fn close_open(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };
        let block = match open {
            OpenBlock::Text(text) => {
                if text.is_empty() {
                    return;
                }
                ContentBlock::Text { text }
            }
            OpenBlock::Thinking { text, signature } => {
                if text.is_empty() {
                    return;
                }
                ContentBlock::Thinking { text, signature }
            }
            OpenBlock::ToolUse { id, name, json } => {
                let input = parse_tool_input(&name, &json);
                ContentBlock::ToolUse { id, name, input }
            }
        };
        self.blocks.push(block);
    }

    fn finish(mut self) -> Vec<ContentBlock> {
        self.close_open();
        self.blocks
    }
}

/// tool_use 의 누적 입력(부분 JSON 문자열)을 [`serde_json::Value`] 로 파싱한다.
///
/// 빈 입력은 인자 없는 호출이므로 빈 객체로 본다. 파싱 실패(모델이 잘못된 JSON 을 낸
/// 경우)는 빈 객체로 대체하고 경고만 남긴다 — 도구가 필수 인자 부재로 에러를 돌려주면
/// 모델이 복구할 수 있다. (CODING_RULES §9: tool_use 입력은 문자열 매칭 말고 JSON 파싱.)
fn parse_tool_input(name: &str, json: &str) -> serde_json::Value {
    if json.trim().is_empty() {
        return serde_json::Value::Object(Default::default());
    }
    match serde_json::from_str(json) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(tool = %name, %error, "tool_use input is not valid JSON; using empty object");
            serde_json::Value::Object(Default::default())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn apply_all(events: Vec<StreamEvent>) -> Vec<ContentBlock> {
        let mut asm = MessageAssembler::default();
        for ev in events {
            asm.apply(ev);
        }
        asm.finish()
    }

    #[test]
    fn assembles_streamed_text_into_one_block() {
        let blocks = apply_all(vec![
            StreamEvent::TextDelta("Hello, ".into()),
            StreamEvent::TextDelta("world".into()),
            StreamEvent::ContentBlockStop,
        ]);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "Hello, world"));
    }

    #[test]
    fn aggregates_tool_use_from_split_json_deltas() {
        // 텍스트 한 블록 + tool_use 한 블록. 입력 JSON 은 여러 delta 로 쪼개져 온다.
        let blocks = apply_all(vec![
            StreamEvent::TextDelta("reading".into()),
            StreamEvent::ToolUseStart {
                id: "call_1".into(),
                name: "read".into(),
            },
            StreamEvent::ToolUseInputDelta {
                id: "call_1".into(),
                partial_json: "{\"path\":".into(),
            },
            StreamEvent::ToolUseInputDelta {
                id: "call_1".into(),
                partial_json: "\"a.rs\"}".into(),
            },
            StreamEvent::ContentBlockStop,
        ]);
        assert_eq!(blocks.len(), 2);
        assert!(matches!(&blocks[0], ContentBlock::Text { text } if text == "reading"));
        match &blocks[1] {
            ContentBlock::ToolUse { id, name, input } => {
                assert_eq!(id, "call_1");
                assert_eq!(name, "read");
                assert_eq!(input, &json!({ "path": "a.rs" }));
            }
            other => panic!("expected tool_use, got {other:?}"),
        }
    }

    #[test]
    fn tool_use_start_auto_closes_previous_block_without_explicit_stop() {
        // ContentBlockStop 없이 텍스트 → tool_use → tool_use 로 전환해도 순서가 보존된다.
        let blocks = apply_all(vec![
            StreamEvent::TextDelta("ok".into()),
            StreamEvent::ToolUseStart {
                id: "c1".into(),
                name: "glob".into(),
            },
            StreamEvent::ToolUseInputDelta {
                id: "c1".into(),
                partial_json: "{}".into(),
            },
            StreamEvent::ToolUseStart {
                id: "c2".into(),
                name: "grep".into(),
            },
            StreamEvent::ToolUseInputDelta {
                id: "c2".into(),
                partial_json: "{\"q\":\"x\"}".into(),
            },
        ]);
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[0], ContentBlock::Text { .. }));
        assert!(
            matches!(&blocks[1], ContentBlock::ToolUse { name, input, .. }
            if name == "glob" && input == &json!({}))
        );
        assert!(
            matches!(&blocks[2], ContentBlock::ToolUse { name, input, .. }
            if name == "grep" && input == &json!({ "q": "x" }))
        );
    }

    #[test]
    fn empty_tool_arguments_become_empty_object() {
        let blocks = apply_all(vec![StreamEvent::ToolUseStart {
            id: "c1".into(),
            name: "list".into(),
        }]);
        assert_eq!(blocks.len(), 1);
        assert!(matches!(&blocks[0], ContentBlock::ToolUse { input, .. } if input == &json!({})));
    }
}

#[cfg(test)]
mod exec_tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::context::NoopContextManager;
    use crate::provider::{CompletionRequest, EventStream, ModelInfo, Provider, ToolSchema};
    use crate::tool::{
        CancellationToken, PermissionGate, PermissionLevel, Tool, ToolContext, ToolOutput,
        ToolRegistry,
    };

    /// stream/count_tokens 는 execute_tool_calls 테스트에서 호출되지 않는다.
    struct StubProvider;
    #[async_trait]
    impl Provider for StubProvider {
        fn id(&self) -> &str {
            "stub"
        }
        fn models(&self) -> &[ModelInfo] {
            &[]
        }
        async fn stream(&self, _request: CompletionRequest) -> Result<EventStream> {
            unreachable!("stream not used in execute_tool_calls tests")
        }
        async fn count_tokens(
            &self,
            _system: Option<&str>,
            _messages: &[Message],
            _tools: &[ToolSchema],
        ) -> Result<u64> {
            Ok(0)
        }
    }

    struct AllowGate;
    #[async_trait]
    impl PermissionGate for AllowGate {
        async fn decide(&self, _tool: &str, _input: &serde_json::Value) -> PermissionLevel {
            PermissionLevel::Allow
        }
    }

    /// Allow + 이름만 echo. `parallel` 로 parallel_safe 여부를 정한다.
    struct EchoTool {
        name: String,
        parallel: bool,
    }
    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn input_schema(&self) -> serde_json::Value {
            json!({ "type": "object" })
        }
        fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
            PermissionLevel::Allow
        }
        fn parallel_safe(&self) -> bool {
            self.parallel
        }
        async fn invoke(&self, _input: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
            ToolOutput::ok(self.name.clone())
        }
    }

    /// parallel_safe 도구. 두 개가 **동시에** 실행돼야만 barrier 를 통과한다 — 순차면 데드락.
    struct BarrierTool {
        name: String,
        barrier: Arc<tokio::sync::Barrier>,
    }
    #[async_trait]
    impl Tool for BarrierTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "barrier"
        }
        fn input_schema(&self) -> serde_json::Value {
            json!({ "type": "object" })
        }
        fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
            PermissionLevel::Allow
        }
        fn parallel_safe(&self) -> bool {
            true
        }
        async fn invoke(&self, _input: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
            self.barrier.wait().await;
            ToolOutput::ok(self.name.clone())
        }
    }

    fn agent_with(tools: ToolRegistry) -> Agent {
        Agent {
            provider: Arc::new(StubProvider),
            tools,
            permissions: Arc::new(AllowGate),
            context: Arc::new(NoopContextManager),
            model: "m".into(),
            system_prompt: String::new(),
            max_tokens: 16,
            effort: None,
            max_tool_iterations: 5,
            tool_ctx: ToolContext {
                workdir: std::env::temp_dir(),
                cancel: CancellationToken::new(),
            },
        }
    }

    fn tool_use(id: &str, name: &str) -> ContentBlock {
        ContentBlock::ToolUse {
            id: id.into(),
            name: name.into(),
            input: json!({}),
        }
    }

    #[tokio::test]
    async fn parallel_safe_tools_run_concurrently() {
        // barrier(2): 두 도구가 동시에 실행돼야 둘 다 진행한다. 순차 실행이면 데드락 →
        // timeout 으로 잡아 실패시킨다(= 병렬 실행을 증명).
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(BarrierTool {
            name: "a".into(),
            barrier: barrier.clone(),
        }));
        reg.register(Arc::new(BarrierTool {
            name: "b".into(),
            barrier: barrier.clone(),
        }));
        let agent = agent_with(reg);
        let assistant = Message::assistant(vec![tool_use("1", "a"), tool_use("2", "b")]);

        let results = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            agent.execute_tool_calls(&assistant, &NullObserver),
        )
        .await
        .expect("must not deadlock — proves concurrent execution")
        .expect("ok");
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn results_keep_tool_use_order_across_mixed_and_unknown() {
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(EchoTool {
            name: "x".into(),
            parallel: true,
        }));
        reg.register(Arc::new(EchoTool {
            name: "s".into(),
            parallel: false,
        }));
        reg.register(Arc::new(EchoTool {
            name: "y".into(),
            parallel: true,
        }));
        let agent = agent_with(reg);
        // 순서: parallel, sequential, unknown, parallel.
        let assistant = Message::assistant(vec![
            tool_use("1", "x"),
            tool_use("2", "s"),
            tool_use("3", "nope"),
            tool_use("4", "y"),
        ]);

        let results = agent
            .execute_tool_calls(&assistant, &NullObserver)
            .await
            .expect("ok");
        let ids: Vec<&str> = results
            .iter()
            .map(|b| match b {
                ContentBlock::ToolResult { tool_use_id, .. } => tool_use_id.as_str(),
                _ => panic!("expected tool_result"),
            })
            .collect();
        // 원래 tool_use 순서가 그대로 보존된다.
        assert_eq!(ids, ["1", "2", "3", "4"]);
        // 미지의 도구(id 3)는 에러 결과.
        assert!(matches!(
            &results[2],
            ContentBlock::ToolResult { is_error: true, content, .. } if content.contains("unknown tool: nope")
        ));
    }

    #[tokio::test]
    async fn denied_ask_tool_aborts_turn() {
        struct DenyGate;
        #[async_trait]
        impl PermissionGate for DenyGate {
            async fn decide(&self, _t: &str, _i: &serde_json::Value) -> PermissionLevel {
                PermissionLevel::Deny
            }
        }
        struct AskTool;
        #[async_trait]
        impl Tool for AskTool {
            fn name(&self) -> &str {
                "danger"
            }
            fn description(&self) -> &str {
                "ask"
            }
            fn input_schema(&self) -> serde_json::Value {
                json!({ "type": "object" })
            }
            fn permission(&self, _i: &serde_json::Value) -> PermissionLevel {
                PermissionLevel::Ask
            }
            async fn invoke(&self, _i: serde_json::Value, _c: &ToolContext) -> ToolOutput {
                ToolOutput::ok("ran")
            }
        }
        let mut reg = ToolRegistry::new();
        reg.register(Arc::new(AskTool));
        let mut agent = agent_with(reg);
        agent.permissions = Arc::new(DenyGate);
        let assistant = Message::assistant(vec![tool_use("1", "danger")]);
        let err = agent
            .execute_tool_calls(&assistant, &NullObserver)
            .await
            .expect_err("deny should abort the turn");
        assert!(matches!(err, Error::PermissionDenied(t) if t == "danger"));
    }

    /// 고정 이벤트 시퀀스를 스트리밍하는 프로바이더.
    struct ScriptProvider {
        events: Vec<StreamEvent>,
    }
    #[async_trait]
    impl Provider for ScriptProvider {
        fn id(&self) -> &str {
            "script"
        }
        fn models(&self) -> &[ModelInfo] {
            &[]
        }
        async fn stream(&self, _request: CompletionRequest) -> Result<EventStream> {
            let evs: Vec<Result<StreamEvent>> = self.events.iter().cloned().map(Ok).collect();
            Ok(Box::pin(futures::stream::iter(evs)))
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

    #[tokio::test]
    async fn tool_use_stop_without_blocks_ends_turn_not_loop() {
        // stop_reason=ToolUse 인데 tool_use 블록이 없는 비정상 응답 → 빈 결과 무한루프
        // (MaxIterations)로 가지 않고 턴이 정상 종료(Ok)되어야 한다.
        let provider = Arc::new(ScriptProvider {
            events: vec![
                StreamEvent::MessageStart { model: "m".into() },
                StreamEvent::MessageStop {
                    stop_reason: StopReason::ToolUse,
                    usage: crate::message::Usage::default(),
                },
            ],
        });
        let agent = Agent {
            provider,
            tools: ToolRegistry::new(),
            permissions: Arc::new(AllowGate),
            context: Arc::new(NoopContextManager),
            model: "m".into(),
            system_prompt: String::new(),
            max_tokens: 16,
            effort: None,
            max_tool_iterations: 5,
            tool_ctx: ToolContext {
                workdir: std::env::temp_dir(),
                cancel: CancellationToken::new(),
            },
        };
        let mut session = Session::new();
        let r = agent
            .run_turn(&mut session, "hi".into(), &NullObserver)
            .await;
        assert!(r.is_ok(), "expected Ok (turn ended), got {r:?}");
    }
}
