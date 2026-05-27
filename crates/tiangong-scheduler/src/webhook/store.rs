use std::path::PathBuf;

use anyhow::{Context, Result};

use super::model::{UpdateWebhookRequest, Webhook, WebhookRun, WebhookRunStatus};

/// Webhook 存储（JSON 文件）
pub struct WebhookStore {
    webhooks_path: PathBuf,
    runs_dir: PathBuf,
}

impl WebhookStore {
    /// 打开或创建存储目录
    pub fn open() -> Result<Self> {
        let base = webhook_dir();
        std::fs::create_dir_all(&base)
            .with_context(|| format!("创建 webhook 目录失败: {}", base.display()))?;
        let runs_dir = base.join("runs");
        std::fs::create_dir_all(&runs_dir)
            .with_context(|| format!("创建运行记录目录失败: {}", runs_dir.display()))?;
        let webhooks_path = base.join("webhooks.json");
        let store = Self {
            webhooks_path,
            runs_dir,
        };
        store.ensure_file()?;
        Ok(store)
    }

    // ── Webhook CRUD ──────────────────────────────────────────────

    pub fn insert(&self, webhook: &Webhook) -> Result<()> {
        let mut webhooks = self.load_webhooks()?;
        webhooks.push(webhook.clone());
        self.save_webhooks(&webhooks)
    }

    pub fn get(&self, id: &str) -> Result<Option<Webhook>> {
        let webhooks = self.load_webhooks()?;
        Ok(webhooks.into_iter().find(|w| w.id == id))
    }

    pub fn list(&self) -> Result<Vec<Webhook>> {
        let mut webhooks = self.load_webhooks()?;
        webhooks.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(webhooks)
    }

    pub fn update(&self, id: &str, req: &UpdateWebhookRequest) -> Result<bool> {
        let mut webhooks = self.load_webhooks()?;
        let now = chrono::Local::now().naive_local().to_string();
        let Some(webhook) = webhooks.iter_mut().find(|w| w.id == id) else {
            return Ok(false);
        };
        webhook.updated_at = now;
        if let Some(ref v) = req.name {
            webhook.name = v.clone();
        }
        if let Some(ref v) = req.description {
            webhook.description = v.clone();
        }
        if let Some(ref v) = req.session_id {
            webhook.session_id = Some(v.clone());
        }
        if let Some(ref v) = req.payload {
            webhook.payload = v.clone();
        }
        if let Some(ref v) = req.secret {
            webhook.secret = Some(v.clone());
        }
        if let Some(v) = req.enabled {
            webhook.enabled = v;
        }
        self.save_webhooks(&webhooks)?;
        Ok(true)
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let mut webhooks = self.load_webhooks()?;
        let before = webhooks.len();
        webhooks.retain(|w| w.id != id);
        if webhooks.len() == before {
            return Ok(false);
        }
        self.save_webhooks(&webhooks)?;
        let run_file = self.runs_dir.join(format!("{id}.json"));
        let _ = std::fs::remove_file(run_file);
        Ok(true)
    }

    // ── WebhookRun CRUD ───────────────────────────────────────────

    pub fn insert_run(&self, run: &WebhookRun) -> Result<()> {
        let mut runs = self.load_runs(&run.webhook_id)?;
        runs.push(run.clone());
        self.save_runs(&run.webhook_id, &runs)
    }

    pub fn update_run_status(
        &self,
        id: &str,
        webhook_id: &str,
        status: &WebhookRunStatus,
        finished_at: Option<&str>,
        result_summary: Option<&str>,
    ) -> Result<bool> {
        let mut runs = self.load_runs(webhook_id)?;
        let Some(run) = runs.iter_mut().find(|r| r.id == id) else {
            return Ok(false);
        };
        run.status = status.clone();
        if let Some(t) = finished_at {
            run.finished_at = Some(t.to_string());
        }
        if let Some(s) = result_summary {
            run.result_summary = Some(s.to_string());
        }
        self.save_runs(webhook_id, &runs)?;
        Ok(true)
    }

    pub fn list_runs(&self, webhook_id: &str, limit: usize) -> Result<Vec<WebhookRun>> {
        let mut runs = self.load_runs(webhook_id)?;
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        runs.truncate(limit);
        Ok(runs)
    }

    // ── 内部方法 ──────────────────────────────────────────────────

    fn ensure_file(&self) -> Result<()> {
        if !self.webhooks_path.exists() {
            atomic_write(&self.webhooks_path, "[]").with_context(|| "初始化 webhooks.json 失败")?;
        }
        Ok(())
    }

    fn load_webhooks(&self) -> Result<Vec<Webhook>> {
        let content = std::fs::read_to_string(&self.webhooks_path)
            .with_context(|| format!("读取 {} 失败", self.webhooks_path.display()))?;
        let webhooks: Vec<Webhook> = serde_json::from_str(&content)
            .with_context(|| format!("解析 {} 失败", self.webhooks_path.display()))?;
        Ok(webhooks)
    }

    fn save_webhooks(&self, webhooks: &[Webhook]) -> Result<()> {
        let content =
            serde_json::to_string_pretty(webhooks).with_context(|| "序列化 webhooks 失败")?;
        atomic_write(&self.webhooks_path, &content)
            .with_context(|| format!("写入 {} 失败", self.webhooks_path.display()))?;
        Ok(())
    }

    fn load_runs(&self, webhook_id: &str) -> Result<Vec<WebhookRun>> {
        let path = self.runs_dir.join(format!("{webhook_id}.json"));
        if !path.exists() {
            return Ok(vec![]);
        }
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("读取 {} 失败", path.display()))?;
        let runs: Vec<WebhookRun> = serde_json::from_str(&content)
            .with_context(|| format!("解析 {} 失败", path.display()))?;
        Ok(runs)
    }

    fn save_runs(&self, webhook_id: &str, runs: &[WebhookRun]) -> Result<()> {
        let path = self.runs_dir.join(format!("{webhook_id}.json"));
        let content = serde_json::to_string_pretty(runs).with_context(|| "序列化 runs 失败")?;
        atomic_write(&path, &content).with_context(|| format!("写入 {} 失败", path.display()))?;
        Ok(())
    }
}

fn webhook_dir() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.join(".tiangong").join("webhooks")
}

/// 原子写入：先写临时文件再 rename，防止写入中断导致文件损坏
fn atomic_write(path: &std::path::Path, content: &str) -> Result<()> {
    let tmp_path = path.with_extension("tmp");
    std::fs::write(&tmp_path, content)
        .with_context(|| format!("写入临时文件 {} 失败", tmp_path.display()))?;
    std::fs::rename(&tmp_path, path)
        .with_context(|| format!("重命名 {} → {} 失败", tmp_path.display(), path.display()))?;
    Ok(())
}
