//! 에이전트 루프 **종단(e2e) 테스트** — fake `Provider`(미리 정해둔 이벤트 스트림)로
//! 네트워크 없이 한 턴을 끝까지 구동한다(CODING_RULES §10). 텍스트 경로와 도구 경로를 모두 본다.
//!
//! 파일명 접두사 `e2e_` 는 커버리지 게이트(`scripts/coverage.sh`)가 이 타깃을 **e2e 티어**로
//! 분류하는 컨벤션이다. 접두사 없는 `tests/*.rs` 는 통합(integration) 티어로 본다.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use scv_core::agent::{Agent, NullObserver};
use scv_core::context::{
    ClearToolResultsManager, ContextManager, LayeredContextManager, NoopContextManager,
    SummarizingContextManager,
};
use scv_core::message::{ContentBlock, Message, Role, StopReason, StreamEvent, Usage};
use scv_core::provider::{CompletionRequest, EventStream, ModelInfo, Provider, ToolSchema};
use scv_core::tool::{
    CancellationToken, PermissionGate, PermissionLevel, Tool, ToolContext, ToolOutput, ToolRegistry,
};
use scv_core::Result;

/// 호출할 때마다 미리 정해둔 이벤트 목록을 하나씩 스트리밍하는 가짜 프로바이더.
struct FakeProvider {
    scripts: Mutex<VecDeque<Vec<StreamEvent>>>,
    models: Vec<ModelInfo>,
}

impl FakeProvider {
    fn new(scripts: Vec<Vec<StreamEvent>>) -> Self {
        Self {
            scripts: Mutex::new(scripts.into()),
            models: vec![ModelInfo {
                id: "fake".into(),
                context_window: 1000,
                max_output_tokens: 1000,
                supports_thinking: false,
            }],
        }
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn id(&self) -> &str {
        "fake"
    }
    fn models(&self) -> &[ModelInfo] {
        &self.models
    }
    async fn stream(&self, _request: CompletionRequest) -> Result<EventStream> {
        let events = self
            .scripts
            .lock()
            .expect("lock")
            .pop_front()
            .unwrap_or_default();
        let stream =
            futures::stream::iter(events.into_iter().map(Ok::<StreamEvent, scv_core::Error>));
        Ok(Box::pin(stream))
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

/// 입력을 그대로 되돌려주는 읽기 전용(Allow) 도구.
struct EchoTool;

#[async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self) -> &str {
        "echo the input back"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
        PermissionLevel::Allow
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    async fn invoke(&self, input: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
        ToolOutput::ok(format!("echoed: {input}"))
    }
}

/// 항상 허용하는 게이트(Allow 도구에는 호출되지 않지만 조립을 위해 필요).
struct AllowGate;

#[async_trait]
impl PermissionGate for AllowGate {
    async fn decide(&self, _tool: &str, _input: &serde_json::Value) -> PermissionLevel {
        PermissionLevel::Allow
    }
}

fn build_agent(provider: Arc<dyn Provider>, cancel: CancellationToken) -> Agent {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    Agent {
        provider,
        tools,
        permissions: Arc::new(AllowGate),
        context: Arc::new(NoopContextManager),
        model: "fake-model".into(),
        system_prompt: "you are a test".into(),
        max_tokens: 1000,
        effort: None,
        max_tool_iterations: 10,
        tool_ctx: ToolContext {
            workdir: std::env::temp_dir(),
            cancel,
        },
    }
}

fn make_agent(scripts: Vec<Vec<StreamEvent>>) -> Agent {
    build_agent(
        Arc::new(FakeProvider::new(scripts)),
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn text_turn_streams_and_finishes() {
    let agent = make_agent(vec![vec![
        StreamEvent::MessageStart {
            model: "fake".into(),
        },
        StreamEvent::TextDelta("Hello".into()),
        StreamEvent::TextDelta(", world".into()),
        StreamEvent::MessageStop {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        },
    ]]);

    let mut session = scv_core::session::Session::new();
    agent
        .run_turn(&mut session, "hi".into(), &NullObserver)
        .await
        .expect("turn ok");

    // [user, assistant]
    assert_eq!(session.messages.len(), 2);
    assert!(matches!(session.messages[0].role, Role::User));
    let assistant = &session.messages[1];
    assert!(matches!(assistant.role, Role::Assistant));
    assert_eq!(assistant.content.len(), 1);
    assert!(matches!(&assistant.content[0], ContentBlock::Text { text } if text == "Hello, world"));
}

#[tokio::test]
async fn pause_turn_resumes_then_finishes() {
    // 서버사이드 도구 일반화(5c): stop=pause_turn 이면 로컬 도구 실행·user 메시지 추가 없이
    // 히스토리를 재전송해 재개하고, 이어진 응답이 end_turn 이면 턴이 정상 종료된다.
    let agent = make_agent(vec![
        // 1차: 서버사이드 도구 실행 중 일시정지.
        vec![
            StreamEvent::MessageStart {
                model: "fake".into(),
            },
            StreamEvent::TextDelta("searching".into()),
            StreamEvent::MessageStop {
                stop_reason: StopReason::PauseTurn,
                usage: Usage::default(),
            },
        ],
        // 2차: 재개되어 최종 답으로 마무리.
        vec![
            StreamEvent::TextDelta("answer".into()),
            StreamEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
        ],
    ]);

    let mut session = scv_core::session::Session::new();
    agent
        .run_turn(&mut session, "find X".into(), &NullObserver)
        .await
        .expect("turn ok");

    // [user, assistant("searching"), assistant("answer")] — pause_turn 은 tool_result user
    // 메시지를 추가하지 않고 히스토리를 그대로 재전송해 재개한다(로컬 도구 실행 없음).
    assert_eq!(session.messages.len(), 3);
    assert!(matches!(session.messages[0].role, Role::User));
    assert!(
        matches!(&session.messages[1].content[0], ContentBlock::Text { text } if text == "searching")
    );
    assert!(
        matches!(&session.messages[2].content[0], ContentBlock::Text { text } if text == "answer")
    );
}

#[tokio::test]
async fn server_tool_blocks_preserved_across_pause_turn() {
    // 서버사이드 web_search(5d): server_tool_use/web_search_tool_result 블록이 assistant 에
    // 보존돼 pause_turn 재개 시 다시 보낼 수 있어야 한다. 재개 후 최종 답으로 종료.
    let agent = make_agent(vec![
        vec![
            StreamEvent::MessageStart {
                model: "fake".into(),
            },
            StreamEvent::ServerToolUse {
                id: "srv_1".into(),
                name: "web_search".into(),
                input: serde_json::json!({ "q": "x" }),
            },
            StreamEvent::ServerToolResult {
                tool_use_id: "srv_1".into(),
                result_type: "web_search_tool_result".into(),
                content: serde_json::json!([]),
            },
            StreamEvent::MessageStop {
                stop_reason: StopReason::PauseTurn,
                usage: Usage::default(),
            },
        ],
        vec![
            StreamEvent::TextDelta("answer".into()),
            StreamEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
        ],
    ]);

    let mut session = scv_core::session::Session::new();
    agent
        .run_turn(&mut session, "find X".into(), &NullObserver)
        .await
        .expect("turn ok");

    // [user, assistant(서버툴 블록 보존), assistant("answer")].
    assert_eq!(session.messages.len(), 3);
    let first = &session.messages[1];
    assert!(first
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ServerToolUse { .. })));
    assert!(first
        .content
        .iter()
        .any(|b| matches!(b, ContentBlock::ServerToolResult { .. })));
}

#[tokio::test]
async fn pause_turn_cap_stops_after_limit() {
    // 매 응답이 pause_turn → 무한 재개 대신 한도(3) 초과 시 Ok 로 종료. 4개 스크립트면
    // 1·2·3 재개 통과 후 4번째 pause 에서 종료(iteration 캡 10 보다 먼저).
    let pause_script = || {
        vec![
            StreamEvent::TextDelta("searching".into()),
            StreamEvent::MessageStop {
                stop_reason: StopReason::PauseTurn,
                usage: Usage::default(),
            },
        ]
    };
    let agent = make_agent(vec![
        pause_script(),
        pause_script(),
        pause_script(),
        pause_script(),
    ]);
    let mut session = scv_core::session::Session::new();
    agent
        .run_turn(&mut session, "go".into(), &NullObserver)
        .await
        .expect("pause 캡 초과 시 Ok 종료");
    // user + assistant×4 = 5.
    assert_eq!(session.messages.len(), 5);
}

#[tokio::test]
async fn tool_turn_executes_and_threads_result_then_finishes() {
    let agent = make_agent(vec![
        // 1번째 호출: 도구를 부른다.
        vec![
            StreamEvent::MessageStart {
                model: "fake".into(),
            },
            StreamEvent::ToolUseStart {
                id: "c1".into(),
                name: "echo".into(),
            },
            StreamEvent::ToolUseInputDelta {
                id: "c1".into(),
                partial_json: "{\"v\":1}".into(),
            },
            StreamEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
            },
        ],
        // 2번째 호출: 도구 결과를 받고 마무리.
        vec![
            StreamEvent::MessageStart {
                model: "fake".into(),
            },
            StreamEvent::TextDelta("done".into()),
            StreamEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
        ],
    ]);

    let mut session = scv_core::session::Session::new();
    agent
        .run_turn(&mut session, "use the tool".into(), &NullObserver)
        .await
        .expect("turn ok");

    // [user, assistant(tool_use), user(tool_result), assistant(text)]
    assert_eq!(session.messages.len(), 4);

    let tool_use = &session.messages[1];
    assert!(matches!(&tool_use.content[0],
        ContentBlock::ToolUse { name, input, .. }
            if name == "echo" && input == &serde_json::json!({ "v": 1 })));

    let result_msg = &session.messages[2];
    assert!(matches!(result_msg.role, Role::User));
    match &result_msg.content[0] {
        ContentBlock::ToolResult {
            tool_use_id,
            content,
            is_error,
        } => {
            assert_eq!(tool_use_id, "c1");
            assert!(!is_error);
            assert!(content.contains("echoed"), "content = {content}");
            assert!(content.contains("\"v\":1"), "content = {content}");
        }
        other => panic!("expected tool_result, got {other:?}"),
    }

    let final_assistant = &session.messages[3];
    assert!(matches!(&final_assistant.content[0], ContentBlock::Text { text } if text == "done"));
}

#[tokio::test]
async fn final_thinking_only_response_is_promoted_to_text() {
    let agent = make_agent(vec![
        vec![
            StreamEvent::MessageStart {
                model: "fake".into(),
            },
            StreamEvent::ToolUseStart {
                id: "c1".into(),
                name: "echo".into(),
            },
            StreamEvent::ToolUseInputDelta {
                id: "c1".into(),
                partial_json: "{}".into(),
            },
            StreamEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                usage: Usage::default(),
            },
        ],
        vec![
            StreamEvent::MessageStart {
                model: "fake".into(),
            },
            StreamEvent::ThinkingDelta("final answer from compat reasoning".into()),
            StreamEvent::MessageStop {
                stop_reason: StopReason::EndTurn,
                usage: Usage::default(),
            },
        ],
    ]);

    let mut session = scv_core::session::Session::new();
    agent
        .run_turn(&mut session, "use the tool".into(), &NullObserver)
        .await
        .expect("turn ok");

    let final_assistant = &session.messages[3];
    assert!(final_assistant
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::Thinking { text, .. }
            if text == "final answer from compat reasoning")));
    assert!(final_assistant
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::Text { text }
            if text == "final answer from compat reasoning")));
}

/// 첫 `TextDelta` 를 낸 뒤 폴링되면 토큰을 끄고 `Pending` 을 반환하는 프로바이더 —
/// 스트림 소비 **도중** 취소를 결정적으로 재현한다(타이밍 sleep 없이).
struct CancelMidStreamProvider {
    cancel: CancellationToken,
    models: Vec<ModelInfo>,
}

impl CancelMidStreamProvider {
    fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            models: vec![ModelInfo {
                id: "cancel-mid".into(),
                context_window: 1000,
                max_output_tokens: 1000,
                supports_thinking: false,
            }],
        }
    }
}

#[async_trait]
impl Provider for CancelMidStreamProvider {
    fn id(&self) -> &str {
        "cancel-mid"
    }
    fn models(&self) -> &[ModelInfo] {
        &self.models
    }
    async fn stream(&self, _request: CompletionRequest) -> Result<EventStream> {
        let cancel = self.cancel.clone();
        let mut first = true;
        let stream = futures::stream::poll_fn(move |_cx| {
            if first {
                first = false;
                std::task::Poll::Ready(Some(Ok::<_, scv_core::Error>(StreamEvent::TextDelta(
                    "partial answer".into(),
                ))))
            } else {
                // 두 번째 폴: 취소를 트리거하고 더는 내보내지 않는다. run_turn 의 select!
                // 가 취소 브랜치로 깨어나 부분 텍스트를 보존한 채 Cancelled 로 끝난다.
                cancel.cancel();
                std::task::Poll::Pending
            }
        });
        Ok(Box::pin(stream))
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

#[tokio::test]
async fn precancelled_token_returns_cancelled_before_streaming() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    // 진입부 체크포인트에서 잡혀 스트림은 소비되지 않는다.
    let agent = build_agent(Arc::new(FakeProvider::new(vec![])), cancel);
    let mut session = scv_core::session::Session::new();

    let err = agent
        .run_turn(&mut session, "hi".into(), &NullObserver)
        .await
        .unwrap_err();
    assert!(matches!(err, scv_core::Error::Cancelled));
    // user 메시지만 남는다(assistant 없음).
    assert_eq!(session.messages.len(), 1);
    assert!(matches!(session.messages[0].role, Role::User));
}

#[tokio::test]
async fn cancel_during_stream_preserves_partial_text() {
    let cancel = CancellationToken::new();
    let agent = build_agent(
        Arc::new(CancelMidStreamProvider::new(cancel.clone())),
        cancel,
    );
    let mut session = scv_core::session::Session::new();

    let err = agent
        .run_turn(&mut session, "hi".into(), &NullObserver)
        .await
        .unwrap_err();
    assert!(matches!(err, scv_core::Error::Cancelled));
    // 중단돼도 모은 부분 텍스트가 보존된다: [user, assistant("partial answer")].
    assert_eq!(session.messages.len(), 2);
    assert!(matches!(&session.messages[1].content[0],
        ContentBlock::Text { text } if text == "partial answer"));
}

// ───────────────────────── 추가 종단 시나리오 ─────────────────────────

/// 비-parallel(순차 실행) Allow 도구 — execute_tool_calls 의 순차 경로를 운동시킨다.
struct SeqTool;
#[async_trait]
impl Tool for SeqTool {
    fn name(&self) -> &str {
        "seq"
    }
    fn description(&self) -> &str {
        "sequential tool"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
        PermissionLevel::Allow
    }
    fn parallel_safe(&self) -> bool {
        false
    }
    async fn invoke(&self, _input: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
        ToolOutput::ok("seq ran")
    }
}

/// Ask 를 요구하는 도구(모달/게이트 결정 대상).
struct AskTool;
#[async_trait]
impl Tool for AskTool {
    fn name(&self) -> &str {
        "danger"
    }
    fn description(&self) -> &str {
        "irreversible"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
        PermissionLevel::Ask
    }
    async fn invoke(&self, _input: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
        ToolOutput::ok("should not run when denied")
    }
}

struct DenyGate;
#[async_trait]
impl PermissionGate for DenyGate {
    async fn decide(&self, _tool: &str, _input: &serde_json::Value) -> PermissionLevel {
        PermissionLevel::Deny
    }
}

/// 임의의 도구 집합·게이트·컨텍스트 관리자로 에이전트를 조립한다.
fn assemble(
    provider: Arc<dyn Provider>,
    tools: ToolRegistry,
    permissions: Arc<dyn PermissionGate>,
    context: Arc<dyn ContextManager>,
    cancel: CancellationToken,
) -> Agent {
    Agent {
        provider,
        tools,
        permissions,
        context,
        model: "fake-model".into(),
        system_prompt: "you are a test".into(),
        max_tokens: 1000,
        effort: None,
        max_tool_iterations: 3,
        tool_ctx: ToolContext {
            workdir: std::env::temp_dir(),
            cancel,
        },
    }
}

fn tool_call_script(name: &str) -> Vec<StreamEvent> {
    vec![
        StreamEvent::MessageStart {
            model: "fake".into(),
        },
        StreamEvent::ToolUseStart {
            id: "c1".into(),
            name: name.into(),
        },
        StreamEvent::ToolUseInputDelta {
            id: "c1".into(),
            partial_json: "{}".into(),
        },
        StreamEvent::MessageStop {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        },
    ]
}

fn text_script(text: &str, input_tokens: u64) -> Vec<StreamEvent> {
    vec![
        StreamEvent::TextDelta(text.into()),
        StreamEvent::MessageStop {
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens,
                ..Usage::default()
            },
        },
    ]
}

#[tokio::test]
async fn summarizing_context_compacts_prefix_across_iterations() {
    // compaction 트리거(last_input_tokens)는 한 run_turn 안의 직전 이터레이션 usage 다 →
    // 도구 턴으로 이터레이션을 2개 만든다. 같은 FakeProvider 를 에이전트·요약기가 공유한다.
    // 스크립트: ① iter0 도구호출(입력 토큰 500 → 임계 100 초과) ② iter1 요약 호출 ③ iter1 에이전트 호출.
    let iter0 = vec![
        StreamEvent::MessageStart {
            model: "fake".into(),
        },
        StreamEvent::ToolUseStart {
            id: "c1".into(),
            name: "echo".into(),
        },
        StreamEvent::ToolUseInputDelta {
            id: "c1".into(),
            partial_json: "{}".into(),
        },
        StreamEvent::MessageStop {
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens: 500, // 임계(100) 초과 → 다음 이터레이션에서 compaction.
                ..Usage::default()
            },
        },
    ];
    let provider = Arc::new(FakeProvider::new(vec![
        iter0,
        text_script("SUMMARY of earlier conversation", 0),
        text_script("final answer", 0),
    ]));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let agent = assemble(
        provider.clone(),
        tools,
        Arc::new(AllowGate),
        Arc::new(SummarizingContextManager::new(
            provider,
            "fake-model".into(),
            100, // threshold_tokens
            1,   // keep_recent
        )),
        CancellationToken::new(),
    );

    let mut session = scv_core::session::Session::new();
    agent
        .run_turn(&mut session, "go".into(), &NullObserver)
        .await
        .expect("turn ok");

    // compaction 은 전송 메시지에만 영향 — 세션 원본은 그대로:
    // [user, assistant(tool_use), user(tool_result), assistant("final answer")].
    assert_eq!(session.messages.len(), 4);
    assert!(matches!(&session.messages[3].content[0],
        ContentBlock::Text { text } if text == "final answer"));
}

#[tokio::test]
async fn summarizer_renders_all_block_kinds_and_truncates() {
    // render_transcript 의 모든 분기(사고·도구 인자·서버툴·초대형 결과 잘림)를 e2e 바이너리에서
    // 태운다 — prepare 를 직접 호출해 folded 앞부분에 모든 블록 종류를 넣는다.
    let provider = Arc::new(FakeProvider::new(vec![text_script("S", 0)]));
    let mgr = SummarizingContextManager::new(provider, "m".into(), 100, 1);
    let old = Message::assistant(vec![
        ContentBlock::Thinking {
            text: "reasoning".into(),
            signature: None,
        },
        ContentBlock::ToolUse {
            id: "c1".into(),
            name: "read".into(),
            input: serde_json::json!({ "path": "a.rs" }),
        },
        ContentBlock::ServerToolUse {
            id: "s1".into(),
            name: "web_search".into(),
            input: serde_json::json!({ "query": "q" }),
        },
    ]);
    let results = Message {
        role: Role::User,
        content: vec![
            ContentBlock::ToolResult {
                tool_use_id: "c1".into(),
                content: "Z".repeat(5000), // > 4000 캡 → 잘림 경로.
                is_error: false,
            },
            ContentBlock::ServerToolResult {
                tool_use_id: "s1".into(),
                result_type: "web_search_tool_result".into(),
                content: serde_json::json!([]),
            },
        ],
    };
    // keep_recent=1 → 앞 2개(old·results)가 folded → render_transcript 가 모든 분기를 탄다.
    let out = mgr
        .prepare(vec![old, results, Message::user("recent")], 500)
        .await
        .expect("prepare ok");
    assert_eq!(out.len(), 2, "summary + 1 recent");
    assert!(matches!(&out[0].content[0],
        ContentBlock::Text { text } if text.contains("summarized")));
}

#[tokio::test]
async fn layered_context_manager_clears_then_summarizes() {
    // LayeredContextManager 의 두 경로를 e2e 바이너리에서 태운다: ① clear 만으로 충분 ② 요약까지.
    let provider = Arc::new(FakeProvider::new(vec![text_script("SUM", 0)]));
    let mgr = LayeredContextManager::new(provider, "m".into(), 100, 1);

    // ① 앞부분이 큰 도구 결과 → clear 로 임계 아래 → 요약 없음(도구 결과 자리표시자 유지).
    let cleared = mgr
        .prepare(
            vec![
                Message {
                    role: Role::User,
                    content: vec![ContentBlock::ToolResult {
                        tool_use_id: "t".into(),
                        content: "Z".repeat(4000),
                        is_error: false,
                    }],
                },
                Message::user("recent"),
            ],
            500,
        )
        .await
        .expect("prepare ok");
    assert_eq!(cleared.len(), 2);
    assert!(matches!(&cleared[0].content[0],
        ContentBlock::ToolResult { content, .. } if content.starts_with("[cleared")));

    // ② 앞부분이 큰 텍스트(재생성 불가) → clear 로 안 줄어 요약 발동.
    let summarized = mgr
        .prepare(
            vec![Message::user("Z".repeat(4000)), Message::user("recent")],
            500,
        )
        .await
        .expect("prepare ok");
    assert!(matches!(&summarized[0].content[0],
        ContentBlock::Text { text } if text.contains("summarized")));

    // 임계 이하면 무동작.
    let untouched = mgr
        .prepare(vec![Message::user("a"), Message::user("b")], 10)
        .await
        .expect("prepare ok");
    assert_eq!(untouched.len(), 2);
}

#[tokio::test]
async fn clear_tool_results_manager_runs_in_loop() {
    // tool 턴 → 이터레이션 1 의 prepare 에서 직전 tool_result 가 정리 대상이 된다.
    let provider = Arc::new(FakeProvider::new(vec![
        tool_call_script("echo"),
        text_script("done", 0),
    ]));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let agent = assemble(
        provider,
        tools,
        Arc::new(AllowGate),
        Arc::new(ClearToolResultsManager::new(1)),
        CancellationToken::new(),
    );
    let mut session = scv_core::session::Session::new();
    agent
        .run_turn(&mut session, "go".into(), &NullObserver)
        .await
        .expect("turn ok");
    assert_eq!(session.messages.len(), 4);
}

#[tokio::test]
async fn unknown_tool_yields_error_result_and_turn_continues() {
    let provider = Arc::new(FakeProvider::new(vec![
        tool_call_script("does-not-exist"),
        text_script("recovered", 0),
    ]));
    let agent = assemble(
        provider,
        ToolRegistry::new(), // 도구 없음 → 미지의 도구.
        Arc::new(AllowGate),
        Arc::new(NoopContextManager),
        CancellationToken::new(),
    );
    let mut session = scv_core::session::Session::new();
    agent
        .run_turn(&mut session, "go".into(), &NullObserver)
        .await
        .expect("turn ok");
    // [user, assistant(tool_use), user(error tool_result), assistant(text)]
    match &session.messages[2].content[0] {
        ContentBlock::ToolResult {
            content, is_error, ..
        } => {
            assert!(is_error);
            assert!(content.contains("unknown tool"), "got: {content}");
        }
        other => panic!("expected error tool_result, got {other:?}"),
    }
}

#[tokio::test]
async fn sequential_tool_executes_through_loop() {
    let provider = Arc::new(FakeProvider::new(vec![
        tool_call_script("seq"),
        text_script("after seq", 0),
    ]));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(SeqTool));
    let agent = assemble(
        provider,
        tools,
        Arc::new(AllowGate),
        Arc::new(NoopContextManager),
        CancellationToken::new(),
    );
    let mut session = scv_core::session::Session::new();
    agent
        .run_turn(&mut session, "go".into(), &NullObserver)
        .await
        .expect("turn ok");
    match &session.messages[2].content[0] {
        ContentBlock::ToolResult { content, .. } => assert_eq!(content, "seq ran"),
        other => panic!("expected tool_result, got {other:?}"),
    }
}

#[tokio::test]
async fn denied_ask_tool_aborts_turn() {
    let provider = Arc::new(FakeProvider::new(vec![tool_call_script("danger")]));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(AskTool));
    let agent = assemble(
        provider,
        tools,
        Arc::new(DenyGate),
        Arc::new(NoopContextManager),
        CancellationToken::new(),
    );
    let mut session = scv_core::session::Session::new();
    let err = agent
        .run_turn(&mut session, "go".into(), &NullObserver)
        .await
        .expect_err("deny aborts the turn");
    assert!(matches!(err, scv_core::Error::PermissionDenied(t) if t == "danger"));
}

#[tokio::test]
async fn turn_reaches_max_iterations() {
    // 매 호출 tool_use 만 → 끝나지 않아 상한(max_tool_iterations=3)에서 MaxIterations.
    let provider = Arc::new(FakeProvider::new(vec![
        tool_call_script("echo"),
        tool_call_script("echo"),
        tool_call_script("echo"),
    ]));
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(EchoTool));
    let agent = assemble(
        provider,
        tools,
        Arc::new(AllowGate),
        Arc::new(NoopContextManager),
        CancellationToken::new(),
    );
    let mut session = scv_core::session::Session::new();
    let err = agent
        .run_turn(&mut session, "loop".into(), &NullObserver)
        .await
        .expect_err("should hit iteration cap");
    assert!(
        matches!(err, scv_core::Error::MaxIterations(3)),
        "got {err:?}"
    );
}
