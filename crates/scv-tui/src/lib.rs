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

const MAX_TOOL_OUTPUT_DISPLAY_CHARS: usize = 12_000;

pub(crate) fn format_tool_output_for_display(content: &str) -> Option<String> {
    let trimmed = content.trim_end();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.chars().count() <= MAX_TOOL_OUTPUT_DISPLAY_CHARS {
        return Some(trimmed.to_string());
    }
    let mut out: String = trimmed
        .chars()
        .take(MAX_TOOL_OUTPUT_DISPLAY_CHARS)
        .collect();
    out.push_str("\n[tool output truncated for display]");
    Some(out)
}

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
            // 추론(thinking) 증분 — 추론을 노출하는 백엔드(Ollama·로컬 모델, Anthropic thinking)
            // 가 흘리는 사고. (OpenAI 정식 API 는 raw reasoning 을 노출하지 않는다.) 흐리게
            // 보여준다(NO_COLOR 면 그대로).
            AgentEvent::Stream(StreamEvent::ThinkingDelta(t)) => {
                if std::env::var_os("NO_COLOR").is_some() {
                    print!("{t}");
                } else {
                    print!("\x1b[2m{t}\x1b[22m");
                }
                let _ = std::io::stdout().flush();
            }
            AgentEvent::Stream(StreamEvent::MessageStop { stop_reason, usage }) => {
                // in/out + 캐시 토큰(쓰기·읽기)까지 노출 → 프롬프트 캐싱 비용 실측(ROADMAP 5b).
                // 캐시 미사용/미지원 프로바이더는 cache_* 가 0 이라 표시되지 않는다.
                let mut line = format!(
                    "\n— stop: {stop_reason:?}, in: {}, out: {}",
                    usage.input_tokens, usage.output_tokens
                );
                if usage.cache_read_tokens > 0 || usage.cache_write_tokens > 0 {
                    line.push_str(&format!(
                        ", cache_read: {}, cache_write: {}",
                        usage.cache_read_tokens, usage.cache_write_tokens
                    ));
                }
                println!("{line}");
            }
            AgentEvent::ToolStart { name } => print!("\n[tool: {name}] "),
            AgentEvent::ToolEnd {
                name,
                content,
                is_error,
            } => {
                if *is_error {
                    print!("[{name} failed] ");
                }
                if let Some(output) = format_tool_output_for_display(content) {
                    print!("\n[{name} output]\n{output}\n");
                }
                let _ = std::io::stdout().flush();
            }
            AgentEvent::PermissionAsked { name } => print!("\n[permission needed: {name}] "),
            AgentEvent::Interrupted => println!("\n[interrupted]"),
            _ => {}
        }
    }
}
