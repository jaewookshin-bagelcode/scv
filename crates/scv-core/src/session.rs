//! 세션 — 대화 트랜스크립트와 그 영속화.
//!
//! 세션은 메시지 히스토리를 들고 있고, 디스크에 JSONL(한 줄당 한 메시지)로 저장한다.
//! 덕분에 `scv --resume <id>` 로 이어서 작업하거나, 사후에 트랜스크립트를 감사할 수 있다.
//!
//! 영속화 백엔드는 [`SessionStore`] trait 으로 추상화한다(파일/메모리/원격 등 교체 가능).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::message::Message;
use crate::Result;

/// 세션 식별자.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub String);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

/// 한 대화 세션.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// 시간순 메시지 히스토리.
    pub messages: Vec<Message>,
}

impl Session {
    pub fn new() -> Self {
        Self { id: SessionId::new(), created_at: chrono::Utc::now(), messages: Vec::new() }
    }

    pub fn push(&mut self, message: Message) {
        self.messages.push(message);
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

/// 세션 영속화 추상. 구현은 `scv-cli`(파일 기반)가 제공한다.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// 세션 전체를 저장(또는 마지막 메시지를 append).
    async fn save(&self, session: &Session) -> Result<()>;

    /// id 로 세션을 로드.
    async fn load(&self, id: &SessionId) -> Result<Session>;

    /// 저장된 세션 목록.
    async fn list(&self) -> Result<Vec<SessionId>>;
}
