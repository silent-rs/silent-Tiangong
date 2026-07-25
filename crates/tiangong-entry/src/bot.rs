//! `tiangong bot` 子命令——直接操作本地文件管理 bot 制品。
//!
//! 复用 `tiangong-bots` crate 的 `BotRuntime`/`BotStore`（与桌面端同源），
//! 直接读写 `~/.tiangong/bots/`，不走 HTTP。风格对齐 `mcp`/`skill` 子命令。
//!
//! 注意：`start` 启动的 bot 进程作为本 CLI 的子进程，随 CLI 退出而停止
//! （`supervisor` 用 `kill_on_drop`）。长期运行请用桌面端。本命令主要面向
//! headless 下的下载/配置/升级/扫码，以及"测试启动是否成功"。

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};

use tiangong_bots::{
    BotId, BotRuntime, BotStore, InvalidBotId, ProgressFn, ProvisionStatus, RegisterBotRequest,
    UpdateBotRequest,
};

use crate::args::{BotArgs, BotSubcommand};

/// 构造共享 bot 运行时（默认路径 `~/.tiangong/bots/bots.json`）。
fn load_runtime() -> Result<(Arc<BotStore>, Arc<BotRuntime>)> {
    let store = Arc::new(BotStore::new().context("加载 bot 配置失败")?);
    let runtime = Arc::new(BotRuntime::new(store.clone()).context("构造 bot 运行时失败")?);
    Ok((store, runtime))
}

/// 解析 bot 实例 ID（友好错误）。
fn parse_bot_id(raw: &str) -> Result<BotId> {
    BotId::try_from(raw).map_err(|err: InvalidBotId| anyhow!("Bot ID 非法：{err}"))
}

pub(crate) fn run_bot_command(args: BotArgs) -> Result<()> {
    let (store, runtime) = load_runtime()?;
    // CLI 需要独立 tokio runtime 驱动 BotRuntime 的 async 方法。
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("创建 tokio runtime 失败")?;
    rt.block_on(run_subcommand(args, store, runtime))
}

async fn run_subcommand(
    args: BotArgs,
    store: Arc<BotStore>,
    runtime: Arc<BotRuntime>,
) -> Result<()> {
    match args.command {
        BotSubcommand::List => cmd_list(&store, &runtime).await,
        BotSubcommand::Available => cmd_available(&runtime).await,
        BotSubcommand::Install {
            artifact_id,
            id,
            version,
        } => cmd_install(&runtime, artifact_id, id, version).await,
        BotSubcommand::Configure {
            id,
            set,
            secret,
            config_file,
            enable,
            disable,
        } => cmd_configure(&store, id, set, secret, config_file, enable, disable),
        BotSubcommand::Show { id } => cmd_show(&store, id),
        BotSubcommand::Start { id } => cmd_start(&store, &runtime, id).await,
        BotSubcommand::Stop { id } => cmd_stop(&runtime, id).await,
        BotSubcommand::Restart { id } => cmd_restart(&store, &runtime, id).await,
        BotSubcommand::Upgrade { id } => cmd_upgrade(&store, &runtime, id).await,
        BotSubcommand::CheckUpdate { artifact_id } => {
            cmd_check_update(&store, &runtime, artifact_id).await
        }
        BotSubcommand::Remove { id } => cmd_remove(&store, &runtime, id).await,
        BotSubcommand::Log { id } => cmd_log(id),
        BotSubcommand::Provision { id } => cmd_provision(&runtime, id).await,
    }
}

// ============================ 子命令实现 ============================

async fn cmd_list(store: &BotStore, runtime: &BotRuntime) -> Result<()> {
    let registered = store.list();
    let local_artifacts = runtime.scan_local_artifacts();

    if registered.is_empty() && local_artifacts.is_empty() {
        println!("暂无已注册 bot 或已安装制品。");
        println!("使用 `tiangong bot available` 查看线上可安装制品。");
        return Ok(());
    }

    // 合并：已注册 bot 优先（含运行状态），未注册的本地制品单独列出。
    let registered_ids: std::collections::HashSet<&str> =
        registered.iter().map(|b| b.id.as_str()).collect();

    if !registered.is_empty() {
        println!("已注册 bot（{}）：", registered.len());
        for bot in &registered {
            let health = runtime.health(&bot.id).await;
            let health_str = format_health(&health);
            let version = read_version_label(&bot.id);
            println!(
                "  {id:<16} {name:<12} v{ver:<10} {enabled} {health}",
                id = bot.id,
                name = bot.artifact_id,
                ver = version,
                enabled = if bot.enabled { "[启用]" } else { "[禁用]" },
                health = health_str,
            );
        }
    }

    let orphans: Vec<_> = local_artifacts
        .iter()
        .filter(|a| !registered_ids.contains(a.id.as_str()))
        .collect();
    if !orphans.is_empty() {
        if !registered.is_empty() {
            println!();
        }
        println!("已安装但未注册的制品（{}）：", orphans.len());
        for artifact in orphans {
            println!(
                "  {id:<16} {name:<12} v{ver:<10}（使用 `tiangong bot configure {id}` 注册）",
                id = artifact.id,
                name = artifact.artifact_id,
                ver = if artifact.version.is_empty() {
                    "未知".to_string()
                } else {
                    artifact.version.clone()
                },
            );
        }
    }
    Ok(())
}

async fn cmd_available(runtime: &BotRuntime) -> Result<()> {
    let index = runtime
        .fetch_index()
        .await
        .context("拉取线上 bots-index 失败")?;
    if index.bots.is_empty() {
        println!("线上暂无可安装制品。");
        return Ok(());
    }
    println!("线上可安装制品（{}）：", index.bots.len());
    for manifest in &index.bots {
        let platform = tiangong_bots::manifest::current_platform_key();
        let supported = manifest.platforms.contains_key(&platform);
        let mark = if supported {
            "✓"
        } else {
            "✗（当前平台无制品）"
        };
        let min = manifest
            .min_app_version
            .as_deref()
            .map(|v| format!(" 最低要求 v{v}"))
            .unwrap_or_default();
        println!(
            "  {id:<12} {name:<16} v{ver:<10}{min} {mark}",
            id = manifest.id,
            name = manifest.name,
            ver = manifest.version,
            min = min,
            mark = mark,
        );
        if !manifest.description.is_empty() {
            println!("    {}", manifest.description);
        }
    }
    Ok(())
}

async fn cmd_install(
    runtime: &BotRuntime,
    artifact_id: String,
    id: Option<String>,
    version: Option<String>,
) -> Result<()> {
    let index = runtime
        .fetch_index()
        .await
        .context("拉取线上 bots-index 失败")?;
    let manifest = pick_manifest(&index, &artifact_id, version.as_deref())?;
    let dest_id_str = id.unwrap_or_else(|| artifact_id.clone());
    let dest_id = parse_bot_id(&dest_id_str)?;

    let progress: ProgressFn = Arc::new(|downloaded, content_len| {
        if content_len > 0 {
            let pct = (downloaded as f64 / content_len as f64 * 100.0).min(100.0);
            eprint!("\r下载中：{downloaded} / {content_len} 字节 ({pct:.1}%)");
        } else {
            eprint!("\r下载中：{downloaded} 字节");
        }
    });

    runtime.install(manifest, &dest_id, Some(progress)).await?;
    eprintln!();
    println!("制品已安装：{dest_id}");
    println!("（尚未注册配置，请使用 `tiangong bot configure {dest_id}` 填写凭证）");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_configure(
    store: &BotStore,
    id: String,
    set: Vec<(String, String)>,
    secret: Vec<(String, String)>,
    config_file: Option<String>,
    enable: bool,
    disable: bool,
) -> Result<()> {
    let id = parse_bot_id(&id)?;
    let mut config_map: BTreeMap<String, serde_json::Value> = BTreeMap::new();

    for (key, value) in set {
        config_map.insert(key, serde_json::json!(value));
    }
    for (key, env_var) in secret {
        let value = std::env::var(&env_var)
            .with_context(|| format!("读取 secret 环境变量失败：{env_var} 未设置"))?;
        config_map.insert(key, serde_json::json!(value));
    }
    if let Some(path) = config_file {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("读取 config 文件失败：{path}"))?;
        let map: BTreeMap<String, serde_json::Value> =
            serde_json::from_str(&raw).context("解析 config 文件 JSON 失败")?;
        config_map.extend(map);
    }

    if store.get(&id).is_some() {
        store.update(&id, UpdateBotRequest { config: config_map })?;
        println!("bot 配置已更新：{id}");
    } else {
        let artifact_id = infer_artifact_id(&id).with_context(|| {
            "bot 未注册且无法推断制品 ID，请先 `tiangong bot install` 安装制品或手动指定 artifact_id"
        })?;
        store.register(RegisterBotRequest {
            id: id.to_string(),
            artifact_id,
            config: config_map,
            enabled: !disable,
        })?;
        println!("bot 配置已注册：{id}");
    }

    if enable {
        store.set_enabled(&id, true)?;
    }
    if disable {
        store.set_enabled(&id, false)?;
    }
    Ok(())
}

fn cmd_show(store: &BotStore, id: String) -> Result<()> {
    let id = parse_bot_id(&id)?;
    let bot = store.get(&id).ok_or_else(|| anyhow!("bot 不存在：{id}"))?;

    let schema = tiangong_bots::cached_schema(&id).unwrap_or_default();
    let masked = mask_secret_config(&bot.config, &schema);

    println!("ID:          {}", bot.id);
    println!("制品:        {}", bot.artifact_id);
    println!("启用状态:    {}", if bot.enabled { "启用" } else { "禁用" });
    println!("创建时间:    {}", bot.created_at);
    println!("更新时间:    {}", bot.updated_at);
    println!("配置:");
    if masked.is_empty() {
        println!("  （无）");
    } else {
        for (key, value) in &masked {
            println!("  {key}: {value}");
        }
    }
    Ok(())
}

async fn cmd_start(store: &BotStore, runtime: &BotRuntime, id: String) -> Result<()> {
    let id = parse_bot_id(&id)?;
    let bot = store.get(&id).ok_or_else(|| anyhow!("bot 不存在：{id}"))?;
    let extra_env = derive_server_env();
    runtime.start(&bot, &extra_env).await?;
    println!("bot 已启动：{id}");
    println!("（提示：bot 进程随本 CLI 退出而停止，长期运行请用桌面端）");
    Ok(())
}

async fn cmd_stop(runtime: &BotRuntime, id: String) -> Result<()> {
    let id = parse_bot_id(&id)?;
    runtime.stop(&id).await?;
    println!("bot 已停止：{id}");
    Ok(())
}

async fn cmd_restart(store: &BotStore, runtime: &BotRuntime, id: String) -> Result<()> {
    let id = parse_bot_id(&id)?;
    runtime.stop(&id).await.ok();
    let bot = store.get(&id).ok_or_else(|| anyhow!("bot 不存在：{id}"))?;
    let extra_env = derive_server_env();
    runtime.start(&bot, &extra_env).await?;
    println!("bot 已重启：{id}");
    Ok(())
}

async fn cmd_upgrade(store: &BotStore, runtime: &BotRuntime, id: String) -> Result<()> {
    let id = parse_bot_id(&id)?;
    let bot = store.get(&id).ok_or_else(|| anyhow!("bot 不存在：{id}"))?;
    let updated = runtime
        .check_update(&bot.artifact_id)
        .await
        .context("检查更新失败")?;
    let Some(manifest) = updated else {
        println!("bot {id} 已是最新版本");
        return Ok(());
    };
    let was_running = runtime.is_running(&id).await;
    let extra_env = if was_running {
        Some(derive_server_env())
    } else {
        None
    };
    runtime.upgrade(&id, manifest, None).await?;
    println!("bot {id} 已升级到最新版本");
    if was_running {
        let bot = store
            .get(&id)
            .ok_or_else(|| anyhow!("升级后 bot 配置丢失：{id}"))?;
        if let Some(env) = extra_env {
            runtime.start(&bot, &env).await?;
            println!("bot {id} 已恢复运行");
        }
    }
    Ok(())
}

async fn cmd_check_update(
    store: &BotStore,
    runtime: &BotRuntime,
    artifact_id: Option<String>,
) -> Result<()> {
    let targets: Vec<String> = match artifact_id {
        Some(id) => vec![id],
        None => {
            let locals = runtime.scan_local_artifacts();
            let registered = store.list();
            let mut ids: Vec<String> = locals.iter().map(|a| a.artifact_id.clone()).collect();
            ids.extend(registered.iter().map(|b| b.artifact_id.clone()));
            ids.sort();
            ids.dedup();
            if ids.is_empty() {
                println!("暂无已安装制品可供检查。");
                return Ok(());
            }
            ids
        }
    };

    for id in &targets {
        match runtime.check_update(id).await {
            Ok(Some(manifest)) => {
                println!("{id}: 有更新 → v{}", manifest.version);
            }
            Ok(None) => {
                println!("{id}: 已是最新");
            }
            Err(err) => {
                println!("{id}: 检查失败（{err}）");
            }
        }
    }
    Ok(())
}

async fn cmd_remove(store: &BotStore, runtime: &BotRuntime, id: String) -> Result<()> {
    let id = parse_bot_id(&id)?;
    if store.get(&id).is_none() {
        return Err(anyhow!("bot 不存在：{id}"));
    }
    runtime.stop(&id).await.ok();
    store.remove(&id)?;
    println!("bot 配置已删除：{id}（已安装制品保留）");
    Ok(())
}

fn cmd_log(id: String) -> Result<()> {
    let id = parse_bot_id(&id)?;
    let log = tiangong_bots::read_log_tail(&id).context("读取 bot 日志失败")?;
    if log.content.is_empty() {
        println!("（暂无日志）");
    } else {
        if log.truncated {
            println!("（日志过长，仅显示尾部内容）");
        }
        print!("{}", log.content);
    }
    Ok(())
}

async fn cmd_provision(runtime: &BotRuntime, id: String) -> Result<()> {
    let id = parse_bot_id(&id)?;
    let session = runtime.provision_begin(&id).await?;

    print_qr(&session.qr_url)?;
    println!();
    println!("请用手机扫码授权，或手动访问：");
    println!("  {}", session.qr_url);
    println!();

    let mut interval = session.interval.max(1);
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        match runtime.provision_poll(&id, &session).await? {
            ProvisionStatus::Pending { retry_after } => {
                if let Some(next) = retry_after {
                    interval = next.max(1);
                }
                eprint!("\r等待扫码授权...");
            }
            ProvisionStatus::Success => {
                eprintln!();
                println!("授权成功");
                return Ok(());
            }
            ProvisionStatus::Expired => {
                eprintln!();
                return Err(anyhow!("扫码会话已过期，请重新执行 provision"));
            }
            ProvisionStatus::Error { message } => {
                eprintln!();
                return Err(anyhow!("扫码授权失败：{message}"));
            }
        }
    }
}

// ============================ 辅助函数 ============================

/// 从 bots-index 中按 artifact_id（及可选版本）选取 manifest。
fn pick_manifest(
    index: &tiangong_bots::manifest::BotsIndex,
    artifact_id: &str,
    version: Option<&str>,
) -> Result<tiangong_bots::manifest::BotManifest> {
    let mut candidates: Vec<_> = index.bots.iter().filter(|m| m.id == artifact_id).collect();
    if candidates.is_empty() {
        return Err(anyhow!("线上 bots-index 未找到制品：{artifact_id}"));
    }
    if let Some(want) = version {
        candidates.retain(|m| m.version == want);
        if candidates.is_empty() {
            return Err(anyhow!(
                "制品 {artifact_id} 无版本 {want}（线上版本见 `tiangong bot available`）"
            ));
        }
        return Ok(candidates[0].clone());
    }
    Ok(candidates[0].clone())
}

/// 从已安装制品的 version.json 推断 artifact_id（用于首次注册时）。
fn infer_artifact_id(id: &BotId) -> Option<String> {
    tiangong_bots::version::read_installed_version(id).map(|v| v.artifact_id)
}

/// 读取本地制品版本标签（用于列表展示）。
fn read_version_label(id: &BotId) -> String {
    tiangong_bots::version::read_installed_version(id)
        .map(|v| v.version)
        .unwrap_or_else(|| "未安装".to_string())
}

/// 格式化健康状态。
fn format_health(health: &tiangong_bots::BotHealth) -> String {
    match health {
        tiangong_bots::BotHealth::Running => "运行中".to_string(),
        tiangong_bots::BotHealth::Stopped => "已停止".to_string(),
        tiangong_bots::BotHealth::MissingArtifact => "未安装".to_string(),
        tiangong_bots::BotHealth::Error { message } => format!("错误：{message}"),
    }
}

/// 从 server.json 读取配置，拼装 bot 启动所需的 TIANGONG_URL/TIANGONG_TOKEN。
///
/// 对齐桌面端 `bot_server_env` 的逻辑（src-tauri/src/commands.rs）。host 为空
/// 或通配地址时回退到 127.0.0.1。
fn derive_server_env() -> BTreeMap<String, String> {
    let config = tiangong_config::load_server_config();
    let host = connect_host(&config.host);
    let mut env = BTreeMap::new();
    env.insert(
        "TIANGONG_URL".into(),
        format!("http://{host}:{}", config.port),
    );
    env.insert(
        "TIANGONG_TOKEN".into(),
        config.auth_token.unwrap_or_default(),
    );
    env
}

/// 规范化监听地址为可连接地址（通配/空 → 127.0.0.1）。
fn connect_host(host: &str) -> String {
    match host.trim() {
        "" | "0.0.0.0" | "::" => "127.0.0.1".to_string(),
        value => value.to_string(),
    }
}

/// 脱敏 config map：schema 中标记为 Secret 的字段值替换为 `***`。
fn mask_secret_config(
    config: &BTreeMap<String, serde_json::Value>,
    schema: &[tiangong_bots::ConfigFieldSchema],
) -> Vec<(String, String)> {
    let secret_keys: std::collections::HashSet<&str> = schema
        .iter()
        .filter(|f| matches!(f.field_type, tiangong_bots::FieldType::Secret))
        .map(|f| f.key.as_str())
        .collect();

    let mut result = Vec::new();
    for (key, value) in config {
        let display = if secret_keys.contains(key.as_str()) {
            "***".to_string()
        } else {
            value.to_string()
        };
        result.push((key.clone(), display));
    }
    result
}

/// 用 qrcode crate 把 URL 渲染为终端 Unicode 二维码（输出到 stdout）。
fn print_qr(url: &str) -> Result<()> {
    use qrcode::QrCode;
    use qrcode::render::unicode;

    let code = QrCode::new(url.as_bytes()).context("生成二维码失败（URL 过长？）")?;
    let image = code
        .render::<unicode::Dense1x2>()
        .dark_color(unicode::Dense1x2::Light)
        .light_color(unicode::Dense1x2::Dark)
        .build();
    print!("{image}");
    Ok(())
}
