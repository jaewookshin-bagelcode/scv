//! `scv-core` — 에이전트의 도메인 모델과 추상 계층.
//!
//! 이 크레이트는 **추상(trait)과 데이터 타입, 그리고 에이전트 루프**만 정의한다.
//! 구체 구현(Anthropic 어댑터, bash 도구 등)은 별도 크레이트가 `scv-core` 의
//! trait 을 구현하는 방식으로 제공한다 → 의존성이 항상 core 를 향한다(의존성 역전).
//!
//! 모듈 지도:
//! - [`message`]      프로바이더 중립 대화 모델 (Message / ContentBlock / StreamEvent)
//! - [`provider`]     LLM 프로바이더 추상 ([`provider::Provider`])
//! - [`tool`]         도구 추상 ([`tool::Tool`]) 과 권한 모델
//! - [`skill`]        스킬 모델과 레지스트리(progressive disclosure)
//! - [`system_prompt`] 계층형 시스템 프롬프트 빌더
//! - [`session`]      세션(대화 트랜스크립트)과 영속화 추상
//! - [`context`]      컨텍스트 윈도 관리 / compaction
//! - [`agent`]        agentic loop — 위 조각들을 묶어 한 턴을 구동

#![warn(missing_debug_implementations, rust_2018_idioms, unreachable_pub)]

pub mod agent;
pub mod context;
pub mod error;
pub mod message;
pub mod provider;
pub mod session;
pub mod skill;
pub mod system_prompt;
pub mod tool;

pub use error::{Error, Result};
