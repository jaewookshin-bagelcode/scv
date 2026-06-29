//! `transcript_search` 도구 — 과거 세션 트랜스크립트(JSONL)에서 **정확 일치** 검색.
//!
//! compaction(요약)은 손실적이라 디테일을 잃을 수 있다. 이 도구는 세션 디렉터리의
//! `<id>.jsonl` 들을 부분문자열로 훑어 **원문 그대로** 되살린다(요약본의 흐릿한 기억이
//! 아니라 정밀 추출 — ARCHITECTURE §4.2 의 `ClearToolResultsManager` 와 짝). 읽기 전용·
//! 부작용 없음 → `Allow` + `parallel_safe`.
//!
//! 검색 루트(세션 디렉터리)는 합성 루트(scv-cli)가 생성 시 주입한다 — 모델이 경로를 정하지
//! 않으므로 workdir 경로 탈출 위험이 없다(질의 문자열만 받는다).

use std::path::PathBuf;

use async_trait::async_trait;
use scv_core::tool::{PermissionLevel, Tool, ToolContext, ToolOutput};

/// 돌려줄 최대 매치 라인 수.
const MAX_MATCHES: usize = 100;
/// 한 매치 라인의 최대 출력 길이.
const MAX_LINE_LEN: usize = 300;

#[derive(Debug)]
pub struct TranscriptSearchTool {
    /// 세션 JSONL 이 쌓이는 디렉터리(설정 `[session].dir`).
    root: PathBuf,
}

impl TranscriptSearchTool {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }
}

#[async_trait]
impl Tool for TranscriptSearchTool {
    fn name(&self) -> &str {
        "transcript_search"
    }

    fn description(&self) -> &str {
        "Search past session transcripts for an exact substring and return matching lines as \
         \"session:lineno: text\". Use to recover precise earlier content lost to summarization."
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Exact substring to find in past transcripts" }
            },
            "required": ["query"],
            "additionalProperties": false
        })
    }

    fn permission(&self, _input: &serde_json::Value) -> PermissionLevel {
        PermissionLevel::Allow
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    async fn invoke(&self, input: serde_json::Value, _ctx: &ToolContext) -> ToolOutput {
        let Some(query) = input.get("query").and_then(|v| v.as_str()) else {
            return ToolOutput::error("missing `query`");
        };
        if query.is_empty() {
            return ToolOutput::error("`query` must not be empty");
        }
        let root = self.root.clone();
        let query = query.to_string();
        // 파일시스템 walk + read 는 blocking → 별도 스레드.
        match tokio::task::spawn_blocking(move || search(&root, &query)).await {
            Ok(out) => ToolOutput::ok(out),
            Err(e) => ToolOutput::error(format!("search task failed: {e}")),
        }
    }
}

/// 세션 디렉터리의 모든 `*.jsonl` 을 줄 단위로 훑어 `query` 부분문자열이 든 줄을 모은다.
/// 세션 디렉터리가 아직 없으면(첫 실행 등) 매치 없음으로 본다.
fn search(root: &PathBuf, query: &str) -> String {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(_) => return "no transcripts yet".to_string(),
    };
    let mut out: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        let sid = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string();
        for (i, line) in content.lines().enumerate() {
            if line.contains(query) {
                let snippet: String = line.chars().take(MAX_LINE_LEN).collect();
                out.push(format!("{sid}:{}: {snippet}", i + 1));
                if out.len() >= MAX_MATCHES {
                    out.push(format!("[stopped at {MAX_MATCHES} matches]"));
                    return out.join("\n");
                }
            }
        }
    }
    if out.is_empty() {
        "no matches".to_string()
    } else {
        out.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scv_core::tool::{CancellationToken, ToolContext};

    fn ctx() -> ToolContext {
        ToolContext {
            workdir: std::env::temp_dir(),
            cancel: CancellationToken::new(),
        }
    }

    fn write_session(dir: &std::path::Path, id: &str, lines: &[&str]) {
        let path = dir.join(format!("{id}.jsonl"));
        std::fs::write(path, lines.join("\n")).unwrap();
    }

    #[tokio::test]
    async fn finds_substring_across_sessions() {
        let dir = std::env::temp_dir().join(format!("scv-ts-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        write_session(
            &dir,
            "sess-a",
            &["{\"role\":\"user\",\"text\":\"deploy the widget\"}"],
        );
        write_session(
            &dir,
            "sess-b",
            &["{\"role\":\"user\",\"text\":\"unrelated\"}"],
        );

        let tool = TranscriptSearchTool::new(dir.clone());
        let out = tool
            .invoke(serde_json::json!({ "query": "widget" }), &ctx())
            .await;
        assert!(!out.is_error);
        assert!(out.content.contains("sess-a:1:"), "got: {}", out.content);
        assert!(!out.content.contains("sess-b"), "got: {}", out.content);

        let none = tool
            .invoke(serde_json::json!({ "query": "nonexistent-zzz" }), &ctx())
            .await;
        assert_eq!(none.content, "no matches");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn missing_dir_reports_no_transcripts() {
        let dir = std::env::temp_dir().join("scv-ts-absent-dir-xyz");
        let _ = std::fs::remove_dir_all(&dir);
        let tool = TranscriptSearchTool::new(dir);
        let out = tool
            .invoke(serde_json::json!({ "query": "x" }), &ctx())
            .await;
        assert!(!out.is_error);
        assert_eq!(out.content, "no transcripts yet");
    }

    #[tokio::test]
    async fn rejects_empty_query() {
        let tool = TranscriptSearchTool::new(std::env::temp_dir());
        assert!(
            tool.invoke(serde_json::json!({ "query": "" }), &ctx())
                .await
                .is_error
        );
        assert!(tool.invoke(serde_json::json!({}), &ctx()).await.is_error);
    }

    #[test]
    fn metadata_is_read_only_and_parallel_safe() {
        let tool = TranscriptSearchTool::new(std::env::temp_dir());
        assert_eq!(tool.name(), "transcript_search");
        assert!(!tool.description().is_empty());
        assert_eq!(tool.input_schema()["type"], "object");
        assert_eq!(
            tool.permission(&serde_json::json!({})),
            PermissionLevel::Allow
        );
        assert!(tool.parallel_safe());
    }

    #[tokio::test]
    async fn skips_non_jsonl_files() {
        let dir = std::env::temp_dir().join(format!("scv-ts-nonjsonl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // .txt 는 건너뛰고, .jsonl 만 검색 대상.
        std::fs::write(dir.join("notes.txt"), "needle here").unwrap();
        write_session(&dir, "sess", &["{\"text\":\"needle here\"}"]);

        let tool = TranscriptSearchTool::new(dir.clone());
        let out = tool
            .invoke(serde_json::json!({ "query": "needle" }), &ctx())
            .await;
        assert!(out.content.contains("sess:1:"), "got: {}", out.content);
        assert!(!out.content.contains("notes"), "got: {}", out.content);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn caps_at_max_matches() {
        let dir = std::env::temp_dir().join(format!("scv-ts-cap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // MAX_MATCHES(100) 를 초과하는 매치 라인 → 상한에서 멈춘다.
        let lines: Vec<String> = (0..150).map(|i| format!("hit {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        write_session(&dir, "big", &refs);

        let tool = TranscriptSearchTool::new(dir.clone());
        let out = tool
            .invoke(serde_json::json!({ "query": "hit" }), &ctx())
            .await;
        assert!(
            out.content.contains("[stopped at 100 matches]"),
            "{}",
            out.content
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
