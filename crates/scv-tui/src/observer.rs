//! 에이전트 루프 → TUI 통지를 채널로 흘리는 [`Observer`].
//!
//! `Observer::on_event` 은 `&self` 라 화면을 직접 못 만진다(§4.5). 그래서 이벤트를
//! mpsc 로 보내고, UI 이벤트 루프가 받아서 렌더한다 — **단방향 관찰**(되먹임 없음).

use async_trait::async_trait;
use scv_core::agent::Observer;
use scv_core::message::AgentEvent;
use tokio::sync::mpsc;

/// [`AgentEvent`] 를 UI 루프로 전달하는 관찰자.
pub(crate) struct ChannelObserver {
    tx: mpsc::UnboundedSender<AgentEvent>,
}

impl ChannelObserver {
    pub(crate) fn new(tx: mpsc::UnboundedSender<AgentEvent>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl Observer for ChannelObserver {
    async fn on_event(&self, event: &AgentEvent) {
        // UI 루프가 사라져 수신자가 닫혔어도 루프는 계속 흘러야 하므로 send 실패는 무시한다
        // (관찰 전용 — 통지 유실이 루프를 막아선 안 된다).
        let _ = self.tx.send(event.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn forwards_events_to_channel() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let obs = ChannelObserver::new(tx);
        obs.on_event(&AgentEvent::ToolStart {
            name: "bash".into(),
        })
        .await;
        match rx.recv().await {
            Some(AgentEvent::ToolStart { name }) => assert_eq!(name, "bash"),
            other => panic!("expected ToolStart, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_after_receiver_dropped_is_silent() {
        let (tx, rx) = mpsc::unbounded_channel();
        drop(rx);
        let obs = ChannelObserver::new(tx);
        // 수신자가 없어도 패닉/블록 없이 조용히 무시되어야 한다.
        obs.on_event(&AgentEvent::Interrupted).await;
    }
}
