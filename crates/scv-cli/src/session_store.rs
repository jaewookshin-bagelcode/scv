//! 파일 기반 세션 저장소 — `SessionStore` 의 구체 구현.
//!
//! 세션마다 `<dir>/<id>.jsonl` 파일에 메시지를 한 줄당 하나씩 저장한다.
//! (이 구현이 합성 루트인 cli 에 있는 이유: core 는 "어디에 저장할지"를 몰라야 한다.)
//!
//! 격리/동시성(ARCHITECTURE.md §4.2 세션 격리):
//! - 다른 세션 id → 다른 파일 → 격리됨.
//! - `save` 가 파일을 통째로 다시 쓰므로(락 없음) **같은 id 를 두 프로세스에서 동시
//!   `--resume`** 하면 나중에 저장한 쪽이 덮어쓴다(데이터 손실). 계획: append-only 쓰기
//!   또는 파일 락으로 안전화.

#![allow(dead_code)] // 스캐폴드: resume 연결 시 사용.

use std::path::PathBuf;

use async_trait::async_trait;
use scv_core::session::{Session, SessionId, SessionStore};
use scv_core::{Error, Result};

#[derive(Debug, Clone)]
pub struct FileSessionStore {
    dir: PathBuf,
}

impl FileSessionStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, id: &SessionId) -> PathBuf {
        self.dir.join(format!("{}.jsonl", id.0))
    }
}

#[async_trait]
impl SessionStore for FileSessionStore {
    async fn save(&self, session: &Session) -> Result<()> {
        tokio::fs::create_dir_all(&self.dir).await.map_err(Error::SessionStore)?;
        // 메시지를 JSONL 로 직렬화.
        let mut buf = String::new();
        for msg in &session.messages {
            buf.push_str(&serde_json::to_string(msg)?);
            buf.push('\n');
        }
        tokio::fs::write(self.path(&session.id), buf).await.map_err(Error::SessionStore)
    }

    async fn load(&self, id: &SessionId) -> Result<Session> {
        let text = tokio::fs::read_to_string(self.path(id)).await.map_err(Error::SessionStore)?;
        let mut session = Session::new();
        session.id = id.clone();
        for line in text.lines().filter(|l| !l.is_empty()) {
            session.messages.push(serde_json::from_str(line)?);
        }
        Ok(session)
    }

    async fn list(&self) -> Result<Vec<SessionId>> {
        let mut ids = Vec::new();
        let mut rd = match tokio::fs::read_dir(&self.dir).await {
            Ok(rd) => rd,
            Err(_) => return Ok(ids), // 디렉터리 없음 = 빈 목록
        };
        while let Some(entry) = rd.next_entry().await.map_err(Error::SessionStore)? {
            if let Some(stem) = entry.path().file_stem().and_then(|s| s.to_str()) {
                ids.push(SessionId(stem.to_string()));
            }
        }
        Ok(ids)
    }
}
