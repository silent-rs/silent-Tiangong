use std::fs;
use std::path::Path;

use tiangong_core::index::{IndexManager, IndexQuery, IndexScope, TurnData};

fn create_temp_workspace(dir: &Path) {
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::create_dir_all(dir.join("src/bin")).unwrap();
    fs::create_dir_all(dir.join("docs")).unwrap();
    fs::create_dir_all(dir.join("target")).unwrap();

    fs::write(
        dir.join("src/main.rs"),
        r#"use std::io;

fn main() {
    println!("hello world");
}

pub fn greet(name: &str) -> String {
    format!("hello {}", name)
}

pub struct App {
    name: String,
}

pub enum Status {
    Active,
    Inactive,
}

pub trait Service {
    fn run(&self);
}

pub const VERSION: &str = "1.0.0";

mod inner {
    fn helper() {}
}

impl App {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string() }
    }
}
"#,
    )
    .unwrap();

    fs::write(
        dir.join("src/bin/cli.rs"),
        r#"fn main() {
    println!("cli tool");
}"#,
    )
    .unwrap();

    fs::write(dir.join("docs/guide.md"), "# Guide\n\nThis is a guide.").unwrap();

    // target 目录应该被跳过
    fs::write(dir.join("target/debug.bin"), "binary content").unwrap();

    // 二进制文件应该被跳过
    fs::write(dir.join("image.png"), "fake png").unwrap();
}

#[test]
fn test_workspace_full_scan_and_search() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path();
    create_temp_workspace(workspace);

    let manager = IndexManager::new().unwrap();
    let count = manager.full_scan(workspace).unwrap();

    assert!(count >= 3, "至少索引 3 个源文件，实际: {}", count);

    let hits = manager
        .search(
            workspace,
            &IndexQuery::new("greet").with_scope(IndexScope::Workspace),
        )
        .unwrap();
    assert!(!hits.is_empty(), "搜索 'greet' 应有结果");
    assert_eq!(hits[0].source, IndexScope::Workspace);

    let path_hit = manager
        .search(
            workspace,
            &IndexQuery::new("main.rs").with_scope(IndexScope::Workspace),
        )
        .unwrap();
    assert!(
        path_hit.iter().any(|h| h.path.contains("main.rs")),
        "搜索路径 'main.rs' 应有结果"
    );
}

#[test]
fn test_workspace_skip_dirs_and_binary_files() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path();
    create_temp_workspace(workspace);

    let manager = IndexManager::new().unwrap();
    let _count = manager.full_scan(workspace).unwrap();

    let hits = manager
        .search(
            workspace,
            &IndexQuery::new("debug.bin").with_scope(IndexScope::Workspace),
        )
        .unwrap();
    assert!(hits.is_empty(), "target 目录下的文件应被跳过");

    let png_hits = manager
        .search(
            workspace,
            &IndexQuery::new("fake png").with_scope(IndexScope::Workspace),
        )
        .unwrap();
    assert!(png_hits.is_empty(), "png 文件应被跳过");
}

#[test]
fn test_workspace_rust_symbol_search() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path();
    create_temp_workspace(workspace);

    let manager = IndexManager::new().unwrap();
    let _ = manager.full_scan(workspace);

    // 搜索函数名
    let fn_hits = manager
        .search(
            workspace,
            &IndexQuery::new("greet").with_scope(IndexScope::Workspace),
        )
        .unwrap();
    assert!(!fn_hits.is_empty(), "搜索函数 'greet' 应有结果");

    // 搜索结构体名
    let struct_hits = manager
        .search(
            workspace,
            &IndexQuery::new("App").with_scope(IndexScope::Workspace),
        )
        .unwrap();
    assert!(!struct_hits.is_empty(), "搜索结构体 'App' 应有结果");

    // 搜索枚举名
    let enum_hits = manager
        .search(
            workspace,
            &IndexQuery::new("Status").with_scope(IndexScope::Workspace),
        )
        .unwrap();
    assert!(!enum_hits.is_empty(), "搜索枚举 'Status' 应有结果");

    // 搜索 trait 名
    let trait_hits = manager
        .search(
            workspace,
            &IndexQuery::new("Service").with_scope(IndexScope::Workspace),
        )
        .unwrap();
    assert!(!trait_hits.is_empty(), "搜索 trait 'Service' 应有结果");

    // 搜索常量名
    let const_hits = manager
        .search(
            workspace,
            &IndexQuery::new("VERSION").with_scope(IndexScope::Workspace),
        )
        .unwrap();
    assert!(!const_hits.is_empty(), "搜索常量 'VERSION' 应有结果");
}

#[test]
fn test_workspace_update_and_remove_file() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path();
    create_temp_workspace(workspace);

    let manager = IndexManager::new().unwrap();
    let _count = manager.full_scan(workspace).unwrap();

    // 新增文件
    let new_file = workspace.join("src/lib.rs");
    fs::write(&new_file, "pub fn new_feature() {}").unwrap();
    manager.update_file(workspace, &new_file).unwrap();

    let hits = manager
        .search(
            workspace,
            &IndexQuery::new("new_feature").with_scope(IndexScope::Workspace),
        )
        .unwrap();
    assert!(!hits.is_empty(), "增量更新后应能搜到新函数");

    // 删除文件（从索引中移除）
    manager.remove_file(workspace, "src/lib.rs").unwrap();
}

#[test]
fn test_session_index_turn_and_search() {
    let manager = IndexManager::new().unwrap();
    let session_id = scru128::new().to_string();

    let turn = TurnData {
        turn_id: scru128::new().to_string(),
        workspace_id: "test-workspace".to_string(),
        role: "user".to_string(),
        content: "如何在 Rust 中实现一个异步 HTTP 服务器？".to_string(),
        topics: vec!["rust".to_string(), "async".to_string(), "http".to_string()],
        entity_names: vec!["tokio".to_string(), "hyper".to_string()],
    };

    manager.index_turn(&session_id, &turn).unwrap();

    let assistant_turn = TurnData {
        turn_id: scru128::new().to_string(),
        workspace_id: "test-workspace".to_string(),
        role: "assistant".to_string(),
        content: "可以使用 tokio 和 hyper 库来构建异步 HTTP 服务器".to_string(),
        topics: vec!["rust".to_string(), "http".to_string()],
        entity_names: vec!["tokio".to_string(), "hyper".to_string()],
    };

    manager.index_turn(&session_id, &assistant_turn).unwrap();

    let hits = manager
        .search_session(&session_id, "HTTP 服务器", 10)
        .unwrap();
    assert!(!hits.is_empty(), "搜索 Session 应有结果");
    assert_eq!(hits.len(), 2, "应有两条 turn 记录");

    let rust_hits = manager.search_session(&session_id, "tokio", 10).unwrap();
    assert!(!rust_hits.is_empty(), "搜索实体名 'tokio' 应有结果");

    let count = manager.session_turn_count(&session_id).unwrap();
    assert_eq!(count, 2, "turn 计数应为 2");
}

#[test]
fn test_session_finalize() {
    let manager = IndexManager::new().unwrap();
    let session_id = scru128::new().to_string();

    let turn = TurnData {
        turn_id: scru128::new().to_string(),
        workspace_id: "test-workspace".to_string(),
        role: "user".to_string(),
        content: "测试 finalize 功能".to_string(),
        topics: vec!["test".to_string()],
        entity_names: vec![],
    };

    manager.index_turn(&session_id, &turn).unwrap();
    manager.finalize_session_index(&session_id).unwrap();

    // 验证 meta.json 已写入
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| ".".into());
    let meta_path = home
        .join(".tiangong")
        .join("index")
        .join("sessions")
        .join(&session_id)
        .join("meta.json");
    assert!(meta_path.exists(), "finalize 后应写入 meta.json");
}

#[test]
fn test_workspace_entry_count() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = temp.path();
    create_temp_workspace(workspace);

    let manager = IndexManager::new().unwrap();
    let count = manager.full_scan(workspace).unwrap();

    let queried = manager.workspace_entry_count(workspace).unwrap();
    assert_eq!(count, queried, "full_scan 返回值应与 entry_count 一致");
}
