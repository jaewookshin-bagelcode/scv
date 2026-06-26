//! 대화형 권한 게이트 — 정적 정책(설정)과 TUI 승인 프롬프트를 **합성**한다(§4.3).
//!
//! 합성 규칙(`decide`):
//! 1. 먼저 안쪽 정적 게이트(`StaticPermissionGate`, 설정 기반)에 묻는다.
//!    - 설정이 `Allow`/`Deny` 로 확정하면 사용자에게 **묻지 않고** 그대로 따른다
//!      (예: `[permissions].tools.bash = "allow"` 면 자동 승인).
//! 2. 정적 게이트가 `Ask` 면 그때 사용자에게 모달로 승인을 요청한다.
//!
//! **fail-closed**(§4.3): UI 루프가 없거나(채널 닫힘) 응답 채널이 드롭되면 `Ask` 를
//! 돌려준다 — 에이전트 루프는 `Allow` 만 실행하므로 이는 "승인 못 받음 → 거부"가 된다.
//! 승인이 곧 사용 조건이라, 우회 경로로 `Allow` 를 만들어내지 않는다.

use std::sync::Arc;

use async_trait::async_trait;
use scv_core::tool::{PermissionGate, PermissionLevel};
use tokio::sync::{mpsc, oneshot};

/// UI 루프로 보내는 승인 요청. `reply` 로 사용자의 결정을 게이트에 되돌린다.
pub(crate) struct PermissionRequest {
    pub(crate) tool: String,
    /// 도구 입력(JSON). 모달이 "무엇을 승인하는지" 요약을 보여줄 때 쓴다(app::summarize_input).
    pub(crate) input: serde_json::Value,
    pub(crate) reply: oneshot::Sender<PermissionLevel>,
}

/// 정적 게이트 + 대화형 프롬프트 합성 게이트.
pub(crate) struct InteractivePermissionGate {
    inner: Arc<dyn PermissionGate>,
    tx: mpsc::Sender<PermissionRequest>,
}

impl InteractivePermissionGate {
    pub(crate) fn new(inner: Arc<dyn PermissionGate>, tx: mpsc::Sender<PermissionRequest>) -> Self {
        Self { inner, tx }
    }
}

#[async_trait]
impl PermissionGate for InteractivePermissionGate {
    async fn decide(&self, tool: &str, input: &serde_json::Value) -> PermissionLevel {
        // 1. 정적 정책이 확정하면 사용자에게 묻지 않는다.
        match self.inner.decide(tool, input).await {
            PermissionLevel::Allow => return PermissionLevel::Allow,
            PermissionLevel::Deny => return PermissionLevel::Deny,
            PermissionLevel::Ask => {}
        }
        // 2. Ask → 사용자 승인 요청.
        let (reply_tx, reply_rx) = oneshot::channel();
        let req = PermissionRequest {
            tool: tool.to_string(),
            input: input.clone(),
            reply: reply_tx,
        };
        if self.tx.send(req).await.is_err() {
            // UI 루프 없음 → fail-closed.
            return PermissionLevel::Ask;
        }
        // 사용자 응답을 기다린다. 채널이 드롭되면(모달이 닫힘 등) fail-closed.
        reply_rx.await.unwrap_or(PermissionLevel::Ask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 고정 레벨을 돌려주는 가짜 정적 게이트.
    struct FixedGate(PermissionLevel);

    #[async_trait]
    impl PermissionGate for FixedGate {
        async fn decide(&self, _tool: &str, _input: &serde_json::Value) -> PermissionLevel {
            self.0
        }
    }

    fn gate(
        inner: PermissionLevel,
    ) -> (InteractivePermissionGate, mpsc::Receiver<PermissionRequest>) {
        let (tx, rx) = mpsc::channel(4);
        (
            InteractivePermissionGate::new(Arc::new(FixedGate(inner)), tx),
            rx,
        )
    }

    #[tokio::test]
    async fn static_allow_resolves_without_prompting() {
        let (g, mut rx) = gate(PermissionLevel::Allow);
        let level = g.decide("bash", &serde_json::json!({})).await;
        assert_eq!(level, PermissionLevel::Allow);
        // 사용자에게 묻지 않았어야 한다.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn static_deny_resolves_without_prompting() {
        let (g, mut rx) = gate(PermissionLevel::Deny);
        let level = g.decide("bash", &serde_json::json!({})).await;
        assert_eq!(level, PermissionLevel::Deny);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn ask_prompts_user_and_returns_their_choice() {
        for choice in [PermissionLevel::Allow, PermissionLevel::Deny] {
            let (g, mut rx) = gate(PermissionLevel::Ask);
            // 가짜 UI: 요청을 받아 사용자의 선택을 되돌린다.
            let ui = tokio::spawn(async move {
                let req = rx.recv().await.expect("request arrives");
                assert_eq!(req.tool, "write");
                req.reply.send(choice).expect("reply");
            });
            let level = g.decide("write", &serde_json::json!({"path": "a"})).await;
            assert_eq!(level, choice);
            ui.await.unwrap();
        }
    }

    #[tokio::test]
    async fn ask_with_closed_ui_channel_fails_closed() {
        // 수신자(UI)가 없으면 send 가 실패 → Ask(거부).
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let g = InteractivePermissionGate::new(Arc::new(FixedGate(PermissionLevel::Ask)), tx);
        let level = g.decide("bash", &serde_json::json!({})).await;
        assert_eq!(level, PermissionLevel::Ask);
    }

    #[tokio::test]
    async fn ask_with_dropped_reply_fails_closed() {
        // UI 가 요청은 받았지만 응답 없이 reply 를 드롭하면 Ask(거부).
        let (g, mut rx) = gate(PermissionLevel::Ask);
        let ui = tokio::spawn(async move {
            let req = rx.recv().await.expect("request arrives");
            drop(req.reply); // 응답하지 않고 드롭
        });
        let level = g.decide("bash", &serde_json::json!({})).await;
        assert_eq!(level, PermissionLevel::Ask);
        ui.await.unwrap();
    }
}
