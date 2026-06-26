//! scv-tools 통합 테스트 — 내장 도구를 공개 `Tool`/`ToolRegistry` 경계로 검증한다.
//! 실제 임시 워크스페이스에 대해 read/write/edit/glob/grep 을 엮어 돌린다(integration 티어 —
//! 파일명에 `e2e_` 접두사가 없다, CODING_RULES §10).

use scv_core::tool::{CancellationToken, PermissionLevel, ToolContext, ToolOutput, ToolRegistry};
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
