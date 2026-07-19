//! Core 注册表：`session_id -> TiangongCore` 的线程安全映射。
//!
//! 单独抽出便于 host 复用同一份 registry 语义（poison 恢复、iter 访问）。

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};

use tiangong_core::core::TiangongCore;

/// 注册表存储（host 可直接持有，也可经 [`crate::CoreManager`] 间接访问）。
pub type CoreRegistry = HashMap<String, TiangongCore>;

/// 锁守卫：poison 恢复后暴露的 `&mut CoreRegistry`。
pub struct CoreRegistryGuard<'a> {
    guard: MutexGuard<'a, CoreRegistry>,
}

impl<'a> CoreRegistryGuard<'a> {
    /// 加锁并对 poison 做恢复（与现有 host 代码一致：记 warn 后继续）。
    pub fn lock(registry: &'a Mutex<CoreRegistry>) -> Self {
        let guard = match registry.lock() {
            Ok(guard) => guard,
            Err(error) => {
                tracing::warn!(error = %error, "Core 注册表锁已损坏，恢复后继续");
                error.into_inner()
            }
        };
        Self { guard }
    }

    pub fn get(&self, session_id: &str) -> Option<&TiangongCore> {
        self.guard.get(session_id)
    }

    pub fn contains_key(&self, session_id: &str) -> bool {
        self.guard.contains_key(session_id)
    }

    pub fn insert(&mut self, session_id: String, core: TiangongCore) {
        self.guard.insert(session_id, core);
    }

    pub fn remove(&mut self, session_id: &str) -> Option<TiangongCore> {
        self.guard.remove(session_id)
    }

    pub fn iter(&self) -> std::collections::hash_map::Iter<'_, String, TiangongCore> {
        self.guard.iter()
    }

    pub fn iter_mut(&mut self) -> std::collections::hash_map::IterMut<'_, String, TiangongCore> {
        self.guard.iter_mut()
    }

    pub fn len(&self) -> usize {
        self.guard.len()
    }

    pub fn is_empty(&self) -> bool {
        self.guard.is_empty()
    }

    pub fn keys(&self) -> std::collections::hash_map::Keys<'_, String, TiangongCore> {
        self.guard.keys()
    }
}
