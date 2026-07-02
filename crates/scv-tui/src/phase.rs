//! 진행 phase 상태머신 + 스피너 — **순수**(부작용 없음, ARCHITECTURE §4.5).
//!
//! TUI 이벤트 루프는 [`AgentEvent`] 를 받아 [`Phase`] 를 도출하고, 도출된 phase 로
//! 상태줄/스피너를 그린다. 전이 자체는 단위 테스트가 가능하도록 화면과 분리한다
//! (functional core / imperative shell, CODING_RULES §4.1).

use scv_core::message::{AgentEvent, StopReason, StreamEvent};

/// 한 턴이 흐르는 동안의 진행 단계(ARCHITECTURE §4.5 전이도).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum Phase {
    /// 턴이 없는 대기 상태(입력 프롬프트 표시).
    #[default]
    Idle,
    /// 요청을 보냈고 아직 아무 증분도 오지 않음.
    Waiting,
    /// 모델이 사고(thinking)를 흘리는 중.
    Thinking,
    /// 모델이 답변 텍스트를 흘리는 중(스피너 대신 스트림 텍스트를 보여준다).
    Responding,
    /// 모델이 도구 호출을 요청함(`MessageStop{ToolUse}`) — 실행 직전.
    ToolPending,
    /// 도구 실행 중(이름).
    RunningTool(String),
    /// `Ask` 도구가 사용자 승인을 기다리는 중(이름) — 모달 표시.
    AwaitingPermission(String),
    /// 사용자 인터럽트로 중단됨.
    Interrupted,
}

impl Phase {
    /// `event` 를 받아 다음 phase 를 도출한다. 화면 상태에 영향 없는 이벤트는 현재를 유지.
    pub(crate) fn next(&self, event: &AgentEvent) -> Phase {
        match event {
            AgentEvent::Stream(StreamEvent::MessageStart { .. }) => Phase::Waiting,
            AgentEvent::Stream(StreamEvent::ThinkingDelta(_)) => Phase::Thinking,
            AgentEvent::Stream(StreamEvent::TextDelta(_)) => Phase::Responding,
            AgentEvent::Stream(StreamEvent::MessageStop {
                stop_reason: StopReason::ToolUse,
                ..
            }) => Phase::ToolPending,
            // 도구 없이 끝난 응답의 MessageStop 은 루프가 곧 턴을 끝내므로 현재 유지.
            AgentEvent::Stream(StreamEvent::MessageStop { .. }) => self.clone(),
            AgentEvent::PermissionAsked { name } => Phase::AwaitingPermission(name.clone()),
            AgentEvent::ToolStart { name } => Phase::RunningTool(name.clone()),
            // 도구 하나가 끝나면 다음 모델 응답을 기다린다.
            AgentEvent::ToolEnd { .. } => Phase::Waiting,
            AgentEvent::Interrupted => Phase::Interrupted,
            // 알 수 없는(향후 추가) 이벤트는 현재 유지(#[non_exhaustive] 대비).
            _ => self.clone(),
        }
    }

    /// 스피너를 돌려야 하는 단계인가? (출력이 아직 안 보이는 단계 — §4.5)
    /// `Responding` 은 스트림 텍스트가 보이므로 스피너를 끈다(스트림과 경쟁 금지).
    pub(crate) fn is_active(&self) -> bool {
        matches!(
            self,
            Phase::Waiting
                | Phase::Thinking
                | Phase::ToolPending
                | Phase::RunningTool(_)
                | Phase::AwaitingPermission(_)
        )
    }

    /// 상태줄에 보일 짧은 설명.
    pub(crate) fn label(&self) -> String {
        match self {
            Phase::Idle => "ready".into(),
            Phase::Waiting => "waiting for model...".into(),
            Phase::Thinking => "thinking...".into(),
            Phase::Responding => "responding...".into(),
            Phase::ToolPending => "preparing tools...".into(),
            Phase::RunningTool(name) => format!("running {name}..."),
            Phase::AwaitingPermission(name) => format!("awaiting approval for {name}"),
            Phase::Interrupted => "interrupted".into(),
        }
    }
}

/// 스피너 글리프 집합 선택.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpinnerStyle {
    /// Braille 점자 애니메이션(유니코드).
    Unicode,
    /// `|/-\` ASCII 폴백(유니코드 미지원 터미널).
    Ascii,
}

const UNICODE_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const ASCII_FRAMES: &[char] = &['|', '/', '-', '\\'];

impl SpinnerStyle {
    /// 설정 문자열(`[ui].spinner`)에서 스타일을 정한다.
    /// - `unicode` / `ascii` → 그대로.
    /// - `auto`(기본) → 로케일이 UTF-8 이면 유니코드, 아니면 ascii.
    /// - 알 수 없는 값 → 경고 후 유니코드.
    pub fn from_config(spinner: &str) -> Self {
        match spinner {
            "ascii" => SpinnerStyle::Ascii,
            "unicode" => SpinnerStyle::Unicode,
            "auto" => {
                if locale_is_utf8() {
                    SpinnerStyle::Unicode
                } else {
                    SpinnerStyle::Ascii
                }
            }
            other => {
                tracing::warn!(spinner = %other, "unknown [ui].spinner; defaulting to unicode");
                SpinnerStyle::Unicode
            }
        }
    }

    fn frames(self) -> &'static [char] {
        match self {
            SpinnerStyle::Unicode => UNICODE_FRAMES,
            SpinnerStyle::Ascii => ASCII_FRAMES,
        }
    }

    /// `tick` 번째 애니메이션 프레임 글리프(주기적으로 순환).
    pub(crate) fn frame(self, tick: usize) -> char {
        let frames = self.frames();
        frames[tick % frames.len()]
    }
}

/// 로케일 환경변수(`LC_ALL`/`LC_CTYPE`/`LANG`)가 UTF-8 인지 본다.
fn locale_is_utf8() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|k| std::env::var(k).ok())
        .any(|v| {
            v.to_ascii_lowercase().contains("utf-8") || v.to_ascii_lowercase().contains("utf8")
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use scv_core::message::Usage;

    fn stream(ev: StreamEvent) -> AgentEvent {
        AgentEvent::Stream(ev)
    }

    #[test]
    fn transitions_follow_stream_events() {
        let p = Phase::Idle;
        let p = p.next(&stream(StreamEvent::MessageStart { model: "m".into() }));
        assert_eq!(p, Phase::Waiting);
        let p = p.next(&stream(StreamEvent::ThinkingDelta("…".into())));
        assert_eq!(p, Phase::Thinking);
        let p = p.next(&stream(StreamEvent::TextDelta("hi".into())));
        assert_eq!(p, Phase::Responding);
    }

    #[test]
    fn tool_use_stop_moves_to_tool_pending_then_running_then_waiting() {
        let p = Phase::Responding.next(&stream(StreamEvent::MessageStop {
            stop_reason: StopReason::ToolUse,
            usage: Usage::default(),
        }));
        assert_eq!(p, Phase::ToolPending);
        let p = p.next(&AgentEvent::ToolStart {
            name: "bash".into(),
        });
        assert_eq!(p, Phase::RunningTool("bash".into()));
        let p = p.next(&AgentEvent::ToolEnd {
            name: "bash".into(),
            content: "ok".into(),
            is_error: false,
        });
        assert_eq!(p, Phase::Waiting);
    }

    #[test]
    fn end_turn_stop_keeps_current_phase() {
        // 도구 없이 끝나는 응답: MessageStop{EndTurn} 은 phase 를 바꾸지 않는다(루프가 Idle 처리).
        let p = Phase::Responding.next(&stream(StreamEvent::MessageStop {
            stop_reason: StopReason::EndTurn,
            usage: Usage::default(),
        }));
        assert_eq!(p, Phase::Responding);
    }

    #[test]
    fn permission_and_interrupt_transitions() {
        let p = Phase::ToolPending.next(&AgentEvent::PermissionAsked {
            name: "write".into(),
        });
        assert_eq!(p, Phase::AwaitingPermission("write".into()));
        let p = p.next(&AgentEvent::Interrupted);
        assert_eq!(p, Phase::Interrupted);
    }

    #[test]
    fn is_active_excludes_responding_idle_interrupted() {
        assert!(Phase::Waiting.is_active());
        assert!(Phase::Thinking.is_active());
        assert!(Phase::RunningTool("x".into()).is_active());
        assert!(Phase::AwaitingPermission("x".into()).is_active());
        assert!(!Phase::Responding.is_active());
        assert!(!Phase::Idle.is_active());
        assert!(!Phase::Interrupted.is_active());
    }

    #[test]
    fn spinner_frames_cycle_and_differ_by_style() {
        let u = SpinnerStyle::Unicode;
        assert_eq!(u.frame(0), '⠋');
        assert_eq!(u.frame(UNICODE_FRAMES.len()), '⠋'); // 순환
        let a = SpinnerStyle::Ascii;
        assert_eq!(a.frame(0), '|');
        assert_eq!(a.frame(1), '/');
        assert_eq!(a.frame(4), '|'); // 4개 주기
    }

    #[test]
    fn spinner_style_from_config_explicit() {
        assert_eq!(SpinnerStyle::from_config("ascii"), SpinnerStyle::Ascii);
        assert_eq!(SpinnerStyle::from_config("unicode"), SpinnerStyle::Unicode);
        // 알 수 없는 값은 유니코드 폴백.
        assert_eq!(SpinnerStyle::from_config("bogus"), SpinnerStyle::Unicode);
    }
}
