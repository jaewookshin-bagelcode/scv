//! 인터랙티브 터미널 UI.
//!
//! 두 가지를 제공한다:
//! 1. [`StreamObserver`] — 에이전트 루프의 `Observer` 를 구현해, 정규화된 스트림
//!    이벤트를 받아 화면에 실시간 렌더링한다. **UI 는 프로바이더를 모른다** —
//!    오직 `scv_core::message::StreamEvent` 만 본다.
//! 2. [`App`] — ratatui 기반 대화 루프(입력창/대화 로그/권한 프롬프트). 스캐폴드.

#![warn(rust_2018_idioms, unreachable_pub)]

use async_trait::async_trait;
use scv_core::agent::Observer;
use scv_core::message::StreamEvent;

/// 스트림 이벤트를 stdout 에 흘려보내는 최소 관찰자(원샷/디버그용).
/// 풀 TUI 에서는 이 자리에 ratatui 위젯 갱신이 들어간다.
#[derive(Debug, Default)]
pub struct StreamObserver;

#[async_trait]
impl Observer for StreamObserver {
    async fn on_event(&self, event: &StreamEvent) {
        match event {
            StreamEvent::TextDelta(t) => print!("{t}"),
            StreamEvent::ToolUseStart { name, .. } => print!("\n[tool: {name}] "),
            StreamEvent::MessageStop { stop_reason, usage } => {
                println!("\n— stop: {stop_reason:?}, out_tokens: {}", usage.output_tokens);
            }
            _ => {}
        }
    }
}

/// ratatui 기반 대화형 앱(스캐폴드).
///
/// 책임:
/// - 입력창에서 사용자 메시지를 받아 `Agent::run_turn` 을 호출
/// - 스트림 이벤트를 대화 로그 패널에 렌더링
/// - 권한 `Ask` 요청을 모달로 띄우고 사용자의 allow/deny 를 게이트에 전달
/// - Esc/Ctrl-C 로 진행 중 턴을 취소(CancellationToken)
#[derive(Debug, Default)]
pub struct App;

impl App {
    pub fn new() -> Self {
        Self
    }

    /// 터미널 raw mode 진입 → 이벤트 루프 → 종료 시 복원.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        // TODO: crossterm enable_raw_mode + ratatui Terminal 셋업,
        //       입력/렌더/취소 루프 구현.
        Ok(())
    }
}
