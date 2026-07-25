//! `tiangong bot` 子命令——直接操作本地文件管理 bot 制品。
//!
//! 复用 `tiangong-bots` crate 的 `BotRuntime`/`BotStore`（与桌面端同源），
//! 直接读写 `~/.tiangong/bots/`，不走 HTTP。风格对齐 `mcp`/`skill` 子命令。
//!
//! start/configure 启动的 bot 作为独立后台进程运行（setsid 脱离会话），
//! 不随 CLI 退出而终止，适合 headless 常驻场景。

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
    // 所有权守卫（issue #286）：Desktop 运行时拒绝 CLI 直接操作 bot，避免双管理者。
    // Server 运行时：阶段 2 将改为经 HTTP 操作 Server；当前阶段 1 暂沿用本地逻辑
    // （Server 与 CLI 不同进程，本地 bots.json 读写仍可能竞争，阶段 2 完成后消除）。
    match tiangong_config::lock::OwnershipLock::current_owner() {
        Some(tiangong_config::lock::OwnerKind::Desktop) => {
            return Err(anyhow!(
                "Desktop 正在运行并独占管理 bot。请在 Desktop 中操作 bot，                 或先退出 Desktop 后再用 CLI。"
            ));
        }
        Some(tiangong_config::lock::OwnerKind::Server) => {
            // 阶段 2 起：改走 Server HTTP API。当前先放行本地逻辑。
        }
        None => {
            // 无管理者：阶段 2 起将提示「请先启动 Server」。
        }
    }
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
        BotSubcommand::List => cmd_list(&store, &runtime),
        BotSubcommand::Available => cmd_available(&runtime).await,
        BotSubcommand::Install {
            artifact_id,
            id,
            version,
        } => cmd_install(&runtime, artifact_id, id, version).await,
        BotSubcommand::Configure { id } => cmd_configure(&store, &runtime, id).await,
        BotSubcommand::Show { id } => cmd_show(&store, id),
        BotSubcommand::Start { id } => cmd_start(&store, id),
        BotSubcommand::Stop { id } => cmd_stop(id),
        BotSubcommand::Restart { id } => cmd_restart(&store, id),
        BotSubcommand::Upgrade { id } => cmd_upgrade(&store, &runtime, id).await,
        BotSubcommand::CheckUpdate { artifact_id } => {
            cmd_check_update(&store, &runtime, artifact_id).await
        }
        BotSubcommand::Remove { id } => cmd_remove(&store, id),
        BotSubcommand::Log { id } => cmd_log(id),
    }
}

// ============================ 子命令实现 ============================

fn cmd_list(store: &BotStore, runtime: &BotRuntime) -> Result<()> {
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
            let health = bot_health(&bot.id);
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

async fn cmd_configure(store: &BotStore, runtime: &BotRuntime, id: String) -> Result<()> {
    use crate::interactive as ui;

    let id = parse_bot_id(&id)?;
    ui::ensure_terminal().with_context(|| {
        "bot configure 需要交互式终端。\n\
         若通过 `cargo tauri dev` 运行，其子进程 stdin 会被重定向导致无 TTY，\n\
         请直接运行编译产物：./target/debug/tiangong bot configure <id>"
    })?;

    // 确定制品 ID 与 schema 来源。
    let existing = store.get(&id);
    let artifact_id = match &existing {
        Some(bot) => bot.artifact_id.clone(),
        None => infer_artifact_id(&id)
            .with_context(|| "bot 未注册且无法推断制品 ID，请先 `tiangong bot install` 安装制品")?,
    };

    // 制品必须已安装（扫码与启动都依赖制品二进制）。
    if !tiangong_bots::paths::bot_artifact_path(&id).exists() {
        return Err(anyhow!(
            "bot 制品未安装：{id}，请先 `tiangong bot install {artifact_id}`"
        ));
    }

    println!("=== Bot 配置向导：{id}（制品 {artifact_id}）===\n");

    // schema 来源：优先本地缓存，否则拉取线上 manifest。
    let schema = match tiangong_bots::cached_schema(&id) {
        Some(s) => s,
        None => {
            eprintln!("本地无 schema 缓存，拉取线上 bots-index...");
            let index = runtime
                .fetch_index()
                .await
                .context("拉取 bots-index 失败")?;
            let manifest = pick_manifest(&index, &artifact_id, None)?;
            manifest.config_schema
        }
    };

    let has_barcode = schema
        .iter()
        .any(|f| matches!(f.field_type, tiangong_bots::FieldType::Barcode));
    let has_manual_fields = schema
        .iter()
        .any(|f| !matches!(f.field_type, tiangong_bots::FieldType::Barcode));

    // 选择配置方式：有扫码字段时优先引导扫码（对齐 Desktop 流程——先授权后注册启动）。
    let use_provision = if has_barcode {
        let options = if has_manual_fields {
            vec!["扫码授权（推荐）", "手工填写凭证"]
        } else {
            vec!["扫码授权"]
        };
        ui::select("选择配置方式", &options)? == 0
    } else {
        println!("该 bot 不支持扫码配置，将逐项填写凭证。");
        false
    };

    if use_provision {
        // 扫码流程：先创建扫码会话并展示二维码（不依赖 bot 常驻运行），
        // 授权成功后再注册/保留配置并启动 bot。对齐 Desktop 的顺序，避免
        // bot 启动时的必填校验在二维码出现前就失败。
        run_provision_flow(runtime, &id).await?;
        // 扫码凭证由 bot 自行保存。此处保留已有手工配置（若有），仅确保 bot 已注册。
        let preserved_config = existing
            .as_ref()
            .map(|b| b.config.clone())
            .unwrap_or_default();
        upsert_bot_config(store, &id, &artifact_id, preserved_config)?;
    } else {
        // 手工流程：以已有配置为初始值，按字段逐项收集（secret 留空保留原值）。
        let current_config = existing
            .as_ref()
            .map(|b| b.config.clone())
            .unwrap_or_default();
        let config_map = prompt_manual_fields(&schema, &current_config)?;
        upsert_bot_config(store, &id, &artifact_id, config_map)?;
    }

    // 配置完成，启动 bot（已在运行则重启以应用新配置）。
    println!();
    let bot = store.get(&id).context("刚配置的 bot 丢失")?;
    if bot_is_running(&id) {
        eprintln!("bot 已在运行，重启以应用新配置...");
        bot_stop(&id)?;
    } else {
        eprintln!("启动 bot...");
    }
    bot_spawn_daemon(&bot)?;
    println!("✅ bot {id} 配置完成并已启动");
    Ok(())
}

/// 逐个提示用户填写非 barcode 字段，返回填好的 config map。
///
/// 已有配置作为初始值：普通字段显示现有值作默认，secret 字段不显示原文，
/// secret 留空表示保留原值。barcode 字段自动跳过（走扫码分支）。
fn prompt_manual_fields(
    schema: &[tiangong_bots::ConfigFieldSchema],
    current: &BTreeMap<String, serde_json::Value>,
) -> Result<BTreeMap<String, serde_json::Value>> {
    use crate::interactive as ui;

    // 以已有配置为基底，逐字段合并用户输入（只覆盖用户明确填写的字段）。
    let mut config_map = current.clone();
    let editable: Vec<&tiangong_bots::ConfigFieldSchema> = schema
        .iter()
        .filter(|f| !matches!(f.field_type, tiangong_bots::FieldType::Barcode))
        .collect();

    if editable.is_empty() {
        println!("（无可手工配置的字段）");
        return Ok(config_map);
    }

    println!("请按提示填写各字段（直接回车保留当前值/跳过）：\n");
    for field in editable {
        if let Some(help) = &field.help {
            println!("  💡 {help}");
        }
        let label = if field.required {
            format!("{} *", field.label)
        } else {
            field.label.clone()
        };
        let current_value = current.get(&field.key);

        match &field.field_type {
            tiangong_bots::FieldType::Secret => {
                // secret 不回显；留空保留原值（若有），否则删除。
                let prompt = if current_value.is_some() {
                    format!("{label}（留空保留原值，不回显）")
                } else {
                    format!("{label}（不回显）")
                };
                let raw = ui::password(&prompt)?;
                if raw.trim().is_empty() {
                    if field.required && current_value.is_none() {
                        return Err(anyhow!("必填字段不能为空：{}", field.label));
                    }
                    // 留空：保留原值（current 已含），无需操作。
                } else {
                    config_map.insert(field.key.clone(), serde_json::json!(raw.trim()));
                }
            }
            tiangong_bots::FieldType::String => {
                let default = current_value
                    .and_then(|v| v.as_str())
                    .or_else(|| field.default.as_ref().and_then(|v| v.as_str()))
                    .unwrap_or("");
                let raw = ui::input(&label, default)?;
                if raw.trim().is_empty() && field.required && current_value.is_none() {
                    return Err(anyhow!("必填字段不能为空：{}", field.label));
                }
                if !raw.trim().is_empty() {
                    config_map.insert(field.key.clone(), serde_json::json!(raw.trim()));
                }
            }
            tiangong_bots::FieldType::Boolean => {
                let default = current_value
                    .and_then(|v| v.as_bool())
                    .or_else(|| field.default.as_ref().and_then(|v| v.as_bool()))
                    .unwrap_or(false);
                let val = ui::confirm(&label, default)?;
                config_map.insert(field.key.clone(), serde_json::json!(val));
            }
            tiangong_bots::FieldType::Select { options } => {
                let opts: Vec<&str> = options.iter().map(|s| s.as_str()).collect();
                // 默认选中当前值对应的选项。
                let default_idx = current_value
                    .and_then(|v| v.as_str())
                    .and_then(|s| options.iter().position(|o| o == s))
                    .unwrap_or(0);
                let idx = ui::select_with_default(&label, &opts, default_idx)?;
                config_map.insert(field.key.clone(), serde_json::json!(options[idx]));
            }
            tiangong_bots::FieldType::Barcode => {
                // 已过滤，不会到达。
            }
        }
        println!();
    }
    Ok(config_map)
}

/// 注册或更新 bot 配置（已存在则 update，否则 register）。
fn upsert_bot_config(
    store: &BotStore,
    id: &BotId,
    artifact_id: &str,
    config_map: BTreeMap<String, serde_json::Value>,
) -> Result<()> {
    if store.get(id).is_some() {
        store.update(id, UpdateBotRequest { config: config_map })?;
        println!("bot 配置已更新：{id}");
    } else {
        store.register(RegisterBotRequest {
            id: id.to_string(),
            artifact_id: artifact_id.to_string(),
            config: config_map,
            // 配置完即启动，注册时默认启用。
            enabled: true,
        })?;
        println!("bot 配置已注册：{id}");
    }
    Ok(())
}

/// 扫码配置流程：渲染二维码 + 轮询授权状态（configure 扫码分支与 provision 命令共用）。
async fn run_provision_flow(runtime: &BotRuntime, id: &BotId) -> Result<()> {
    let session = runtime.provision_begin(id).await?;

    print_qr(&session.qr_url)?;
    println!();
    println!("请用手机扫码授权，或手动访问：");
    println!("  {}", session.qr_url);
    println!();

    let mut interval = session.interval.max(1);
    loop {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        match runtime.provision_poll(id, &session).await? {
            ProvisionStatus::Pending { retry_after } => {
                if let Some(next) = retry_after {
                    interval = next.max(1);
                }
                eprint!("\r等待扫码授权...");
            }
            ProvisionStatus::Success => {
                eprintln!();
                println!("✅ 授权成功，凭证已由 bot 自行保存");
                return Ok(());
            }
            ProvisionStatus::Expired => {
                eprintln!();
                return Err(anyhow!("扫码会话已过期，请重新执行"));
            }
            ProvisionStatus::Error { message } => {
                eprintln!();
                return Err(anyhow!("扫码授权失败：{message}"));
            }
        }
    }
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

fn cmd_start(store: &BotStore, id: String) -> Result<()> {
    let id = parse_bot_id(&id)?;
    let bot = store.get(&id).ok_or_else(|| anyhow!("bot 不存在：{id}"))?;

    // 已在运行则拒绝（基于 PID 文件跨进程判断，避免重复拉起）。
    if bot_is_running(&id) {
        return Err(anyhow!("bot 已在运行：{id}"));
    }

    bot_spawn_daemon(&bot)?;
    println!("bot 已在后台启动：{id}（PID 见 ~/.tiangong/bots/{id}/bot.pid）");
    Ok(())
}

fn cmd_stop(id: String) -> Result<()> {
    let id = parse_bot_id(&id)?;
    bot_stop(&id)?;
    println!("bot 已停止：{id}");
    Ok(())
}

fn cmd_restart(store: &BotStore, id: String) -> Result<()> {
    let id = parse_bot_id(&id)?;
    bot_stop(&id).ok();
    let bot = store.get(&id).ok_or_else(|| anyhow!("bot 不存在：{id}"))?;
    bot_spawn_daemon(&bot)?;
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
    let was_running = bot_is_running(&id);
    // daemon 模式下 bot 不在进程内监督表，runtime.upgrade 的内部 stop 是空操作，
    // 必须先手动停止后台进程，否则运行中的二进制文件可能被占用导致替换失败。
    if was_running {
        bot_stop(&id)?;
    }
    runtime.upgrade(&id, manifest, None).await?;
    println!("bot {id} 已升级到最新版本");
    if was_running {
        // 升级后重新读取配置（artifact_id 不变，但确保用最新 store 状态）。
        let bot = store
            .get(&id)
            .ok_or_else(|| anyhow!("升级后 bot 配置丢失：{id}"))?;
        bot_spawn_daemon(&bot)?;
        println!("bot {id} 已恢复运行");
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

fn cmd_remove(store: &BotStore, id: String) -> Result<()> {
    let id = parse_bot_id(&id)?;
    if store.get(&id).is_none() {
        return Err(anyhow!("bot 不存在：{id}"));
    }
    bot_stop(&id).ok();
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

/// bot 健康状态（基于 PID 文件的跨进程判断）。
enum BotHealthStatus {
    Running,
    Stopped,
    MissingArtifact,
}

/// 格式化健康状态。
fn format_health(health: &BotHealthStatus) -> String {
    match health {
        BotHealthStatus::Running => "运行中".to_string(),
        BotHealthStatus::Stopped => "已停止".to_string(),
        BotHealthStatus::MissingArtifact => "未安装".to_string(),
    }
}

/// 读取 bot.pid 记录的 PID；文件不存在或无效返回 None。
fn read_pid(id: &BotId) -> Option<u32> {
    let path = tiangong_bots::paths::bot_pid_path(id);
    let content = std::fs::read_to_string(path).ok()?;
    content.trim().parse::<u32>().ok()
}

/// 判断进程是否存活（仅 Unix；非 Unix 始终返回 false）。
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // kill(pid, 0) 不发信号，仅做存在性检查。
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(not(unix))]
fn process_alive(_pid: u32) -> bool {
    false
}

/// bot 是否正在后台运行（基于 bot.pid + 进程存活检查）。
fn bot_is_running(id: &BotId) -> bool {
    read_pid(id).is_some_and(process_alive)
}

/// bot 健康状态：综合制品存在性 + PID 存活判断。
fn bot_health(id: &BotId) -> BotHealthStatus {
    if bot_is_running(id) {
        return BotHealthStatus::Running;
    }
    if tiangong_bots::paths::bot_artifact_path(id).exists() {
        BotHealthStatus::Stopped
    } else {
        BotHealthStatus::MissingArtifact
    }
}

/// 以后台 daemon 方式启动 bot 二进制：setsid 脱离会话、stdin/null、
/// stdout/stderr 追加到 bot.log、写 bot.pid。
///
/// 与桌面端 supervisor 的进程内监督不同——此处 bot 作为独立后台进程运行，
/// 不随 CLI 退出而终止。崩溃不自动重启（用户据日志决定）。
/// PID 文件与 supervisor 共用 bot.pid，确保 CLI 与桌面端互斥管理同一 bot。
fn bot_spawn_daemon(bot: &tiangong_bots::BotConfig) -> Result<()> {
    use std::fs::OpenOptions;
    use std::process::{Command, Stdio};

    let id = &bot.id;
    let artifact = tiangong_bots::paths::bot_artifact_path(id);
    if !artifact.exists() {
        return Err(anyhow!("bot 制品未安装：{id}（{}）", artifact.display()));
    }

    // bot 通过 server 的 /api/v1/messages 收发消息，启动前必须确保 server 已运行。
    // 未运行则自动以后台 daemon 方式拉起（tiangong server --daemon）。
    let server_config = ensure_server_running()?;
    eprintln!(
        "Server 已就绪：{}:{}",
        connect_host(&server_config.host),
        server_config.port
    );

    // 按缓存 schema 注入凭证环境变量（对齐 BotRuntime::start 的 bot_env 逻辑）。
    let schema = tiangong_bots::cached_schema(id).unwrap_or_default();
    tiangong_bots::management::validate_bot_config_fields(&schema, &bot.config)?;
    let mut env = tiangong_bots::bot_env(bot, &schema);
    // 主程序注入的 TIANGONG_URL/TOKEN 覆盖优先级最高（与 runtime 一致）。
    env.extend(server_env_from_config(&server_config));

    let log_path = tiangong_bots::paths::bot_log_path(id);
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("打开 bot 日志失败：{}", log_path.display()))?;
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("打开 bot 日志失败：{}", log_path.display()))?;

    let mut cmd = Command::new(&artifact);
    cmd.stdin(Stdio::null()).stdout(stdout).stderr(stderr);
    for (key, value) in &env {
        cmd.env(key, value);
    }

    // Unix：setsid 脱离父进程会话，避免 CLI 退出波及 bot。
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    // Windows：独立进程组 + 无窗口。
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("后台启动 bot 失败：{id}"))?;
    let pid = child.id();

    // 写 PID 文件（与 supervisor 的 bot.pid 共用）。
    let pid_path = tiangong_bots::paths::bot_pid_path(id);
    if let Some(parent) = pid_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&pid_path, pid.to_string())
        .with_context(|| format!("写入 PID 文件失败：{}", pid_path.display()))?;

    // 短暂等待后确认进程未立即退出，避免因配置错误/制品损坏导致崩溃却报告成功。
    // 用 try_wait（非阻塞）探测：已退出则报错并读日志尾部，仍在运行则 detach。
    std::thread::sleep(Duration::from_millis(500));
    if let Ok(Some(_status)) = child.try_wait() {
        let _ = std::fs::remove_file(&pid_path);
        let log_tail = tiangong_bots::read_log_tail(id)
            .map(|l| l.content)
            .unwrap_or_default();
        let hint = if log_tail.trim().is_empty() {
            String::from("（无日志输出）")
        } else {
            log_tail.lines().next_back().unwrap_or("").to_string()
        };
        return Err(anyhow!(
            "bot {id} 启动后立即退出，可能配置错误或制品损坏。最近日志：{hint}"
        ));
    }
    // 仍在运行：detach（std::process::Child drop 不 kill 子进程，daemon 继续）。
    // 注意：不能 wait 到结束，否则 CLI 阻塞；此处主动放弃句柄，daemon 后台运行。
    // 进程退出后会变 zombie，由系统（launchd）或后续 CLI 调用清理。
    std::mem::forget(child);

    Ok(())
}

/// 停止后台 bot 进程：读 bot.pid → SIGTERM（Unix）→ 清理 PID 文件。
///
/// PID 文件不存在或进程已退出视为已停止（幂等）。Windows 暂不支持信号停止。
fn bot_stop(id: &BotId) -> Result<()> {
    let Some(pid) = read_pid(id) else {
        return Ok(());
    };

    #[cfg(unix)]
    {
        if process_alive(pid) {
            let status = std::process::Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .status()
                .context("发送 SIGTERM 失败")?;
            if !status.success() {
                // 进程可能在读 PID 与发信号之间退出，清理残留 PID 文件。
                let _ = std::fs::remove_file(tiangong_bots::paths::bot_pid_path(id));
                return Err(anyhow!("向 PID {pid} 发送 SIGTERM 失败，进程可能已退出"));
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        return Err(anyhow!(
            "非 Unix 平台暂不支持 bot stop，请手动终止进程（PID 见 bot.pid）"
        ));
    }

    // 清理 PID 文件。
    let _ = std::fs::remove_file(tiangong_bots::paths::bot_pid_path(id));
    Ok(())
}

/// 确保 server 已在后台运行：健康检查通过则直接返回；否则以 daemon 方式拉起
/// 并轮询等待健康。
///
/// bot 必须通过 server 的 `/api/v1/messages` 收发消息，server 未运行时 bot
/// 启动后会立即因连接失败而崩溃。对齐桌面端 `ensure_server_running_for_bots`
/// 的语义，但 CLI 模式拉起的是独立 server 进程（`tiangong server --daemon`），
/// 而非桌面端的 embedded server。
fn ensure_server_running() -> Result<tiangong_config::ServerConfig> {
    let config = tiangong_config::load_server_config();
    if server_health_check(&config) {
        return Ok(config);
    }

    eprintln!("Server 未运行，以后台 daemon 方式拉起...");
    tiangong_server::run_daemon(&config.host, config.port, config.auth_token.clone())
        .context("拉起 Server daemon 失败")?;

    // 轮询等待健康（最多 6 秒，对齐桌面端 wait_for_server_health）。
    for _ in 0..30 {
        if server_health_check(&config) {
            // 标记 enabled，确保下次主程序启动时自动拉起。
            if !config.enabled {
                let mut updated = config.clone();
                updated.enabled = true;
                let _ = tiangong_config::save_server_config(&updated);
            }
            return Ok(config);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(anyhow!(
        "Server 已启动但健康检查未通过（{}:{}），bot 无法收发消息",
        connect_host(&config.host),
        config.port
    ))
}

/// Server 健康检查：TCP 连接 host:port 并发 GET /api/v1/health，返回 200 即健康。
///
/// 复刻桌面端 `server_health_check`（src-tauri/src/commands.rs），用裸 TCP
/// 避免引入 HTTP 客户端依赖。
fn server_health_check(config: &tiangong_config::ServerConfig) -> bool {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::net::ToSocketAddrs;

    let host = connect_host(&config.host);
    let Ok(mut addrs) = (host.as_str(), config.port).to_socket_addrs() else {
        return false;
    };
    let Some(addr) = addrs.next() else {
        return false;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(150)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(150)));
    let _ = stream.set_write_timeout(Some(Duration::from_millis(150)));
    let request = format!(
        "GET /api/v1/health HTTP/1.1\r\nHost: {host}:{}\r\nConnection: close\r\n\r\n",
        config.port
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = [0u8; 1024];
    let Ok(len) = stream.read(&mut response) else {
        return false;
    };
    let response = String::from_utf8_lossy(&response[..len]);
    response.starts_with("HTTP/") && response.contains(" 200 ")
}

/// 由 ServerConfig 拼装 bot 启动所需的 TIANGONG_URL/TIANGONG_TOKEN。
///
/// 对齐桌面端 `bot_server_env` 的逻辑（src-tauri/src/commands.rs）。host 为空
/// 或通配地址时回退到 127.0.0.1。
fn server_env_from_config(config: &tiangong_config::ServerConfig) -> BTreeMap<String, String> {
    let host = connect_host(&config.host);
    let mut env = BTreeMap::new();
    env.insert(
        "TIANGONG_URL".into(),
        format!("http://{host}:{}", config.port),
    );
    env.insert(
        "TIANGONG_TOKEN".into(),
        config.auth_token.clone().unwrap_or_default(),
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
