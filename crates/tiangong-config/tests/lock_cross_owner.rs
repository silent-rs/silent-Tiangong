//! Bot 管理所有权锁的跨所有者互斥场景测试（issue #286 Review 要求）。
//!
//! 通过全局 Mutex 串行化（测试共享 TIANGONG_TEST_LOCK_DIR 环境变量，必须串行）。

use std::sync::{Arc, Barrier, Mutex};
use tiangong_config::lock::{OwnerKind, OwnershipLock};

/// 测试串行锁（环境变量是进程全局，并行测试会互相覆盖）。
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn redirect_lock_dir(tmp: &tempfile::TempDir) {
    // SAFETY: 测试串行执行（经 TEST_LOCK 保证），无其他线程并发访问环境变量。
    unsafe {
        std::env::set_var("TIANGONG_TEST_LOCK_DIR", tmp.path());
    }
}

fn clear_lock_dir() {
    unsafe {
        std::env::remove_var("TIANGONG_TEST_LOCK_DIR");
    }
}

/// Desktop 持锁后 Server 获取失败，并准确报告 Desktop 占用。
#[test]
fn desktop_held_blocks_server_and_reports_correctly() {
    let _guard = TEST_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    redirect_lock_dir(&tmp);
    let desktop = OwnershipLock::acquire(OwnerKind::Desktop).unwrap().unwrap();
    match OwnershipLock::acquire(OwnerKind::Server).unwrap() {
        Err(OwnerKind::Desktop) => {}
        other => panic!("期望 Server 获取失败并报告 Desktop 占用，实际: {other:?}"),
    }
    drop(desktop);
    clear_lock_dir();
}

/// Server 持锁后 Desktop 获取失败，并准确报告 Server 占用。
#[test]
fn server_held_blocks_desktop_and_reports_correctly() {
    let _guard = TEST_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    redirect_lock_dir(&tmp);
    let server = OwnershipLock::acquire(OwnerKind::Server).unwrap().unwrap();
    match OwnershipLock::acquire(OwnerKind::Desktop).unwrap() {
        Err(OwnerKind::Server) => {}
        other => panic!("期望 Desktop 获取失败并报告 Server 占用，实际: {other:?}"),
    }
    drop(server);
    clear_lock_dir();
}

/// 释放管理锁后另一方能够获得，且 current_owner 反映正确。
#[test]
fn release_allows_other_to_acquire() {
    let _guard = TEST_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    redirect_lock_dir(&tmp);
    assert_eq!(OwnershipLock::current_owner(), None);
    let server = OwnershipLock::acquire(OwnerKind::Server).unwrap().unwrap();
    assert_eq!(OwnershipLock::current_owner(), Some(OwnerKind::Server));
    drop(server);
    assert_eq!(OwnershipLock::current_owner(), None);
    let desktop = OwnershipLock::acquire(OwnerKind::Desktop).unwrap().unwrap();
    assert_eq!(OwnershipLock::current_owner(), Some(OwnerKind::Desktop));
    drop(desktop);
    clear_lock_dir();
}

/// 并发同时争用：只有一个胜出（OS 锁保证的真正互斥）。
///
/// 失败方报告的占用方不参与断言——它与胜出方写 owner 标识存在良性的 read/write 竞态
/// （owner 内容仅用于展示，互斥由 OS 锁保证）。
#[test]
fn concurrent_acquire_only_one_wins() {
    let _guard = TEST_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    redirect_lock_dir(&tmp);
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for kind in [OwnerKind::Desktop, OwnerKind::Server] {
        let b = Arc::clone(&barrier);
        handles.push(std::thread::spawn(move || {
            OwnershipLock::acquire(kind).unwrap()
        }));
    }
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(winners, 1, "并发争用只应有一个胜出");
    clear_lock_dir();
}
