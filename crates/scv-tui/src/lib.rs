//! 인터랙티브 터미널 UI.
//!
//! 두 가지를 제공한다:
//! 1. [`StreamObserver`] — 에이전트 루프의 `Observer` 를 구현해, 루프 통지([`AgentEvent`])
//!    를 받아 stdout 에 실시간 렌더링한다(원샷/디버그용). **UI 는 프로바이더를 모른다** —
//!    오직 `scv_core::message` 의 중립 이벤트만 본다.
//! 2. [`App`] — ratatui 기반 대화 루프(입력창/대화 로그/권한 모달/진행 표시·인터럽트).
//!    설계는 ARCHITECTURE §4.5.

#![warn(rust_2018_idioms, unreachable_pub)]

mod app;
mod observer;
mod permission;
mod phase;

use std::io::Write;

use async_trait::async_trait;
use scv_core::agent::Observer;
use scv_core::message::{AgentEvent, StreamEvent};

pub use app::{App, MakeProvider};
pub use phase::SpinnerStyle;

/// 루프 통지를 stdout 에 흘려보내는 최소 관찰자(원샷/디버그용).
/// 풀 TUI 경로는 [`App`] 이 ratatui 로 렌더한다.
#[derive(Debug, Default)]
pub struct StreamObserver;

#[async_trait]
impl Observer for StreamObserver {
    async fn on_event(&self, event: &AgentEvent) {
        match event {
            AgentEvent::Stream(StreamEvent::TextDelta(t)) => {
                print!("{t}");
                // TTY 는 줄 단위 버퍼라 개행 전 증분이 안 보인다 → 토큰마다 flush.
                let _ = std::io::stdout().flush();
            }
            // 추론(thinking) 증분 — reasoning 모델(gemma·o-계열 등)은 답 전에 사고를 흘린다.
            // 흐리게 보여준다(NO_COLOR 면 그대로).
            AgentEvent::Stream(StreamEvent::ThinkingDelta(t)) => {
                if std::env::var_os("NO_COLOR").is_some() {
                    print!("{t}");
                } else {
                    print!("\x1b[2m{t}\x1b[22m");
                }
                let _ = std::io::stdout().flush();
            }
            AgentEvent::Stream(StreamEvent::MessageStop { stop_reason, usage }) => {
                println!(
                    "\n— stop: {stop_reason:?}, out_tokens: {}",
                    usage.output_tokens
                );
            }
            AgentEvent::ToolStart { name } => print!("\n[tool: {name}] "),
            AgentEvent::ToolEnd { name, is_error } if *is_error => print!("[{name} failed] "),
            AgentEvent::PermissionAsked { name } => print!("\n[permission needed: {name}] "),
            AgentEvent::Interrupted => println!("\n[interrupted]"),
            _ => {}
        }
    }
}
