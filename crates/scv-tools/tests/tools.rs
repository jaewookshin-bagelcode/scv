//! scv-tools 통합 테스트 — 내장 도구를 공개 `Tool`/`ToolRegistry` 경계로 검증한다.
//! 실제 임시 워크스페이스에 대해 read/write/edit/glob/grep 을 엮어 돌린다(integration 티어 —
//! 파일명에 `e2e_` 접두사가 없다, CODING_RULES §10).

use std::io::{Read, Write};
use std::net::TcpListener;

use scv_core::tool::{
    CancellationToken, PermissionLevel, Tool, ToolContext, ToolOutput, ToolRegistry,
};
use serde_json::json;

fn workspace(tag: &str) -> ToolContext {
    let dir = std::env::temp_dir().join(format!("scv-tools-it-{}-{}", std::process::id(), tag));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    ToolContext {
        workdir: dir.canonicalize().expect("canon"),
        cancel: CancellationToken::new(),
    }
}

async fn invoke(
    reg: &ToolRegistry,
    name: &str,
    input: serde_json::Value,
    ctx: &ToolContext,
) -> ToolOutput {
    let tool = reg
        .get(name)
        .unwrap_or_else(|| panic!("tool `{name}` is registered"));
    tool.invoke(input, ctx).await
}

#[test]
fn default_registry_has_all_builtin_tools() {
    let reg = scv_tools::default_registry();
    for name in ["read", "glob", "grep", "write", "edit", "bash"] {
        assert!(reg.get(name).is_some(), "missing tool: {name}");
    }
}

#[test]
fn registry_exposes_schemas_for_every_tool() {
    // 에이전트 루프는 reg.schemas() 로 도구 스키마를 모아 모델에 보낸다 — 그 경계를 검증한다
    // (각 도구의 name/description/input_schema/schema 접근자를 운동시킨다).
    let reg = scv_tools::default_registry();
    let schemas = reg.schemas();
    assert!(schemas.len() >= 7, "expected all builtin tools");
    for s in &schemas {
        assert!(!s.name.is_empty());
        assert!(!s.description.is_empty());
        assert_eq!(s.input_schema["type"], "object");
    }
}

#[tokio::test]
async fn static_permission_gate_applies_default_and_overrides() {
    use scv_core::tool::PermissionGate;
    // 설정 기반 정적 게이트(합성 루트가 TUI 대화형 게이트와 합성하는 그 타입).
    let gate = scv_tools::StaticPermissionGate::new(PermissionLevel::Ask)
        .with_override("read", PermissionLevel::Allow)
        .with_override("bash", PermissionLevel::Deny);
    let input = json!({});
    assert_eq!(gate.decide("read", &input).await, PermissionLevel::Allow);
    assert_eq!(gate.decide("bash", &input).await, PermissionLevel::Deny);
    // 오버라이드 없는 도구는 기본값(Ask).
    assert_eq!(gate.decide("edit", &input).await, PermissionLevel::Ask);
}

#[test]
fn permission_classes_match_design() {
    let reg = scv_tools::default_registry();
    let empty = json!({});
    // 읽기 전용 = Allow + parallel_safe.
    for name in ["read", "glob", "grep"] {
        let tool = reg.get(name).unwrap();
        assert_eq!(
            tool.permission(&empty),
            PermissionLevel::Allow,
            "{name} should be Allow"
        );
        assert!(tool.parallel_safe(), "{name} should be parallel_safe");
    }
    // 비가역 = Ask.
    for name in ["write", "edit", "bash"] {
        let tool = reg.get(name).unwrap();
        assert_eq!(
            tool.permission(&empty),
            PermissionLevel::Ask,
            "{name} should be Ask"
        );
    }
}

#[tokio::test]
async fn write_then_read_then_edit_round_trip() {
    let reg = scv_tools::default_registry();
    let ctx = workspace("rt");

    let w = invoke(
        &reg,
        "write",
        json!({ "path": "foo.txt", "content": "hello\nworld" }),
        &ctx,
    )
    .await;
    assert!(!w.is_error, "{}", w.content);

    let r = invoke(&reg, "read", json!({ "path": "foo.txt" }), &ctx).await;
    assert!(!r.is_error);
    assert_eq!(r.content, "hello\nworld");

    let e = invoke(
        &reg,
        "edit",
        json!({ "path": "foo.txt", "old_string": "world", "new_string": "rust" }),
        &ctx,
    )
    .await;
    assert!(!e.is_error, "{}", e.content);

    let r2 = invoke(&reg, "read", json!({ "path": "foo.txt" }), &ctx).await;
    assert_eq!(r2.content, "hello\nrust");

    let _ = std::fs::remove_dir_all(&ctx.workdir);
}

#[tokio::test]
async fn glob_and_grep_find_written_files() {
    let reg = scv_tools::default_registry();
    let ctx = workspace("find");
    std::fs::create_dir_all(ctx.workdir.join("src")).unwrap();

    invoke(
        &reg,
        "write",
        json!({ "path": "src/a.rs", "content": "fn main() {}\n" }),
        &ctx,
    )
    .await;

    let g = invoke(&reg, "glob", json!({ "pattern": "**/*.rs" }), &ctx).await;
    assert!(!g.is_error);
    assert!(g.content.contains("src/a.rs"), "glob = {}", g.content);

    let gr = invoke(&reg, "grep", json!({ "pattern": "fn \\w+" }), &ctx).await;
    assert!(!gr.is_error);
    assert!(gr.content.contains("src/a.rs:1:"), "grep = {}", gr.content);

    let _ = std::fs::remove_dir_all(&ctx.workdir);
}

#[tokio::test]
async fn path_escape_is_rejected_across_tools() {
    let reg = scv_tools::default_registry();
    let ctx = workspace("escape");

    let r = invoke(&reg, "read", json!({ "path": "../../etc/hosts" }), &ctx).await;
    assert!(r.is_error);

    let w = invoke(
        &reg,
        "write",
        json!({ "path": "../escape.txt", "content": "x" }),
        &ctx,
    )
    .await;
    assert!(w.is_error);
    assert!(!ctx.workdir.parent().unwrap().join("escape.txt").exists());

    let _ = std::fs::remove_dir_all(&ctx.workdir);
}

#[tokio::test]
async fn bash_runs_command_in_workspace() {
    let reg = scv_tools::default_registry();
    let ctx = workspace("bash");
    let out = invoke(&reg, "bash", json!({ "command": "echo hi" }), &ctx).await;
    assert!(!out.is_error, "{}", out.content);
    assert!(out.content.contains("hi"), "{}", out.content);
    assert!(out.content.contains("[exit: 0]"), "{}", out.content);

    // 비정상 종료는 에러로 표시된다.
    let bad = invoke(&reg, "bash", json!({ "command": "exit 2" }), &ctx).await;
    assert!(bad.is_error);
    let _ = std::fs::remove_dir_all(&ctx.workdir);
}

#[tokio::test]
async fn web_fetch_rejects_bad_url_without_network() {
    let reg = scv_tools::default_registry();
    let ctx = workspace("web");
    // 네트워크 없이 입력 검증 경로만 — egress 는 Ask 권한.
    let tool = reg.get("web_fetch").expect("web_fetch registered");
    assert_eq!(tool.permission(&json!({})), PermissionLevel::Ask);
    assert!(tool.parallel_safe());

    let missing = invoke(&reg, "web_fetch", json!({}), &ctx).await;
    assert!(missing.is_error);
    let bad = invoke(&reg, "web_fetch", json!({ "url": "ftp://x/y" }), &ctx).await;
    assert!(bad.is_error);
    assert!(bad.content.contains("http"));
    let _ = std::fs::remove_dir_all(&ctx.workdir);
}

#[tokio::test]
async fn tools_report_errors_at_the_registry_boundary() {
    // 레지스트리 경계로 각 도구의 에러 처리(누락 인자·미스매치·읽기 실패 등)를 검증한다.
    let reg = scv_tools::default_registry();
    let ctx = workspace("errs");
    std::fs::create_dir_all(ctx.workdir.join("sub")).unwrap();
    std::fs::write(ctx.workdir.join("f.txt"), "hello").unwrap();

    // read: 디렉터리 → read 실패. 누락 path → 에러.
    assert!(
        invoke(&reg, "read", json!({ "path": "sub" }), &ctx)
            .await
            .is_error
    );
    assert!(invoke(&reg, "read", json!({}), &ctx).await.is_error);

    // edit: old_string 미발견 → 에러. 누락 필드 → 에러.
    let e = invoke(
        &reg,
        "edit",
        json!({ "path": "f.txt", "old_string": "zzz", "new_string": "y" }),
        &ctx,
    )
    .await;
    assert!(e.is_error);
    assert!(
        invoke(&reg, "edit", json!({ "path": "f.txt" }), &ctx)
            .await
            .is_error
    );

    // write: 누락 content → 에러.
    assert!(
        invoke(&reg, "write", json!({ "path": "g.txt" }), &ctx)
            .await
            .is_error
    );

    // grep: 잘못된 정규식 → 에러. 매치 없음 → "(no matches)".
    assert!(
        invoke(&reg, "grep", json!({ "pattern": "(" }), &ctx)
            .await
            .is_error
    );
    let none = invoke(&reg, "grep", json!({ "pattern": "zzz-none" }), &ctx).await;
    assert!(!none.is_error);
    assert_eq!(none.content, "(no matches)");

    // glob: 매치 없음 → "(no matches)". 누락 pattern → 에러.
    let g = invoke(&reg, "glob", json!({ "pattern": "**/*.zzz" }), &ctx).await;
    assert_eq!(g.content, "(no matches)");
    assert!(invoke(&reg, "glob", json!({}), &ctx).await.is_error);

    // bash: 빈 명령 → 에러. stderr 도 출력에 담긴다.
    assert!(
        invoke(&reg, "bash", json!({ "command": "  " }), &ctx)
            .await
            .is_error
    );
    let se = invoke(&reg, "bash", json!({ "command": "echo e 1>&2" }), &ctx).await;
    assert!(se.content.contains("[stderr]"), "{}", se.content);

    let _ = std::fs::remove_dir_all(&ctx.workdir);
}

/// GET 한 번을 받아 고정 응답을 돌려주는 1회용 mock 서버. 포트를 반환한다.
fn spawn_http_mock(status_line: &'static str, body: &'static str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut buf = [0u8; 4096];
            let _ = stream.read(&mut buf);
            let resp = format!(
                "{status_line}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes());
        }
    });
    port
}

#[tokio::test]
async fn web_fetch_returns_body_then_reports_http_error() {
    let reg = scv_tools::default_registry();
    let ctx = workspace("webok");

    // 2xx → 본문 반환.
    let port = spawn_http_mock("HTTP/1.1 200 OK", "fetched body");
    let ok = invoke(
        &reg,
        "web_fetch",
        json!({ "url": format!("http://127.0.0.1:{port}/") }),
        &ctx,
    )
    .await;
    assert!(!ok.is_error, "{}", ok.content);
    assert!(ok.content.contains("fetched body"), "{}", ok.content);

    // 비-2xx → 본문째 에러.
    let port2 = spawn_http_mock("HTTP/1.1 404 Not Found", "nope");
    let err = invoke(
        &reg,
        "web_fetch",
        json!({ "url": format!("http://127.0.0.1:{port2}/") }),
        &ctx,
    )
    .await;
    assert!(err.is_error);
    assert!(err.content.contains("404"), "{}", err.content);

    let _ = std::fs::remove_dir_all(&ctx.workdir);
}

#[tokio::test]
async fn transcript_search_finds_substring_in_sessions() {
    // transcript_search 는 세션 dir 주입이 필요해 default_registry 에 없다 — 직접 만든다.
    let dir = std::env::temp_dir().join(format!("scv-tools-ts-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("sess.jsonl"),
        "{\"role\":\"user\",\"text\":\"deploy the widget\"}\n",
    )
    .unwrap();

    let tool = scv_tools::TranscriptSearchTool::new(dir.clone());
    assert_eq!(tool.name(), "transcript_search");
    assert_eq!(tool.permission(&json!({})), PermissionLevel::Allow);
    let ctx = workspace("ts");
    let out = tool.invoke(json!({ "query": "widget" }), &ctx).await;
    assert!(!out.is_error);
    assert!(out.content.contains("sess:1:"), "{}", out.content);

    let empty = tool.invoke(json!({ "query": "" }), &ctx).await;
    assert!(empty.is_error);

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&ctx.workdir);
}
