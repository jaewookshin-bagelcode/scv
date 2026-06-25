//! 프로젝트 진입 컨텍스트 로더 — `AGENTS.md` 탐색 체인.
//!
//! scv 가 대상 프로젝트에서 시동할 때 진입 컨텍스트 문서를 찾아 시스템 프롬프트의
//! project-context 레이어(ARCHITECTURE.md §4.1)에 합성한다. **새 파일 포맷을 만들지
//! 않고 다른 에이전트 도구와 같은 파일(`AGENTS.md`)을 그대로 읽어** 호환된다.
//!
//! (합성 루트인 cli 에 있는 이유: core 는 "어디서 컨텍스트를 읽을지"를 몰라야 한다 —
//! SessionStore 가 cli 에 있는 것과 같은 이유.)

#![allow(dead_code)] // 스캐폴드: 시스템 프롬프트 합성에 연결되면 사용.

use std::path::Path;

/// 진입 컨텍스트를 찾아 병합한 문자열을 돌려준다(없으면 None).
///
/// 탐색 체인(더 구체적인 것이 상위를 덧붙이거나 덮음, 충돌 시 더 가까운 것 우선):
///   repo 루트 `AGENTS.md`
///     → 하위 디렉터리 `AGENTS.md`(디렉터리 스코프)
///     → 사용자 전역 `~/.config/scv/AGENTS.md`
///     → 폴백 `CLAUDE.md`
///
/// 신규 이름(WORKER.md 등)은 도입하지 않는다. scv 고유 별칭이 필요하면 오버라이드로만
/// 인식하고 캐노니컬 출처는 `AGENTS.md` 로 둔다.
pub fn load(_cwd: &Path) -> Option<String> {
    // TODO: 위 탐색 체인 구현.
    //   1) cwd 에서 위로 올라가며 repo 루트의 AGENTS.md 탐색(.git 경계까지)
    //   2) 하위 디렉터리 AGENTS.md 는 해당 디렉터리에서 작업할 때 스코프로 합침
    //   3) ~/.config/scv/AGENTS.md(전역) 병합
    //   4) 어느 단계든 AGENTS.md 가 없으면 같은 위치의 CLAUDE.md 로 폴백
    None
}
