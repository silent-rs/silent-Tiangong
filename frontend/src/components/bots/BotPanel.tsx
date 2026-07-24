import { useCallback, useEffect, useState } from 'react';
import {
  api,
  type BotConfig,
  type BotHealth,
  type BotManifest,
  type LocalArtifact,
} from '../../api/tauri';
import { Button } from '../ui/button';
import { Badge } from '../ui/badge';
import { Card, CardContent } from '../ui/card';
import {
  ArrowUpCircle,
  Bot as BotIcon,
  Download,
  FileText,
  Loader2,
  MessageSquareText,
  Play,
  RefreshCw,
  Settings as SettingsIcon,
  Square,
  Trash2,
  TriangleAlert,
} from 'lucide-react';
import { useToast } from '../Toast';
import { BotFormDialog } from './BotFormDialog';
import { BotLogDialog } from './BotLogDialog';
import { BotPushTargetsDialog } from './BotPushTargetsDialog';

/** 已安装或已配置的 Bot 条目。 */
interface BotEntry {
  id: string;
  artifactId: string;
  local: LocalArtifact | null;
  manifest: BotManifest | null;
  configured: BotConfig | null;
}

interface ParsedVersion {
  core: [bigint, bigint, bigint];
  prerelease: string[] | null;
}

const SEMVER_PATTERN =
  /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;

function parseVersion(version: string): ParsedVersion | null {
  const match = SEMVER_PATTERN.exec(version);
  if (!match) return null;
  return {
    core: [BigInt(match[1]), BigInt(match[2]), BigInt(match[3])],
    prerelease: match[4]?.split('.') ?? null,
  };
}

function isVersionNewer(remoteVersion: string, localVersion: string): boolean {
  const remote = parseVersion(remoteVersion);
  const local = parseVersion(localVersion);
  if (!remote || !local) return remoteVersion !== localVersion;

  for (let index = 0; index < remote.core.length; index += 1) {
    if (remote.core[index] !== local.core[index]) {
      return remote.core[index] > local.core[index];
    }
  }

  if (!remote.prerelease) return local.prerelease !== null;
  if (!local.prerelease) return false;

  const count = Math.max(remote.prerelease.length, local.prerelease.length);
  for (let index = 0; index < count; index += 1) {
    const remotePart = remote.prerelease[index];
    const localPart = local.prerelease[index];
    if (remotePart === localPart) continue;
    if (remotePart === undefined) return false;
    if (localPart === undefined) return true;

    const remoteNumeric = /^\d+$/.test(remotePart);
    const localNumeric = /^\d+$/.test(localPart);
    if (remoteNumeric && localNumeric) return BigInt(remotePart) > BigInt(localPart);
    if (remoteNumeric !== localNumeric) return !remoteNumeric;
    return remotePart > localPart;
  }
  return false;
}

/** Bot 列表面板——合并线上目录、本地制品和已保存配置。 */
export function BotPanel() {
  const { showSuccess, showError } = useToast();
  const [entries, setEntries] = useState<BotEntry[]>([]);
  const [available, setAvailable] = useState<BotManifest[]>([]);
  const [healthMap, setHealthMap] = useState<Record<string, BotHealth>>({});
  const [isLoading, setIsLoading] = useState(false);
  const [onlineError, setOnlineError] = useState<string | null>(null);
  const [installingBotId, setInstallingBotId] = useState<string | null>(null);
  const [installProgress, setInstallProgress] = useState<number | null>(null);

  // 编辑/配置表单。
  // bot 非空 → 编辑已有配置；bot 为空 → 配置未配置的本地制品。
  const [formArtifact, setFormArtifact] = useState<LocalArtifact | null>(null);
  const [formBot, setFormBot] = useState<BotConfig | null>(null);
  const [logBotId, setLogBotId] = useState<string | null>(null);
  const [pushTargetBot, setPushTargetBot] = useState<{ id: string; name: string } | null>(null);

  const [upgrading, setUpgrading] = useState<Record<string, boolean>>({});

  const load = useCallback(async () => {
    setIsLoading(true);
    try {
      const [localResult, botsResult, onlineResult] = await Promise.allSettled([
        api.botScanLocal(),
        api.botList(),
        api.botAvailable(),
      ]);

      const local = localResult.status === 'fulfilled' ? localResult.value : [];
      const bots = botsResult.status === 'fulfilled' ? botsResult.value : [];
      const manifests = onlineResult.status === 'fulfilled' ? onlineResult.value.bots : [];
      if (localResult.status === 'rejected') {
        console.error('扫描本地 Bot 失败:', localResult.reason);
        showError('加载 Bot 失败', String(localResult.reason));
      } else if (botsResult.status === 'rejected') {
        console.error('加载 Bot 配置失败:', botsResult.reason);
        showError('加载 Bot 失败', String(botsResult.reason));
      }

      if (onlineResult.status === 'rejected') {
        console.error('加载线上 Bot 目录失败:', onlineResult.reason);
        setOnlineError(String(onlineResult.reason));
      } else {
        setOnlineError(null);
      }

      const botById = new Map(bots.map((b) => [b.id, b]));
      const manifestById = new Map(manifests.map((manifest) => [manifest.id, manifest]));
      const merged: BotEntry[] = local.map((l) => ({
        id: l.id,
        artifactId: l.artifact_id,
        local: l,
        manifest: manifestById.get(l.artifact_id) ?? null,
        configured: botById.get(l.id) ?? null,
      }));

      const localIds = new Set(local.map((l) => l.id));
      for (const bot of bots) {
        if (!localIds.has(bot.id)) {
          merged.push({
            id: bot.id,
            artifactId: bot.artifact_id,
            local: null,
            manifest: manifestById.get(bot.artifact_id) ?? null,
            configured: bot,
          });
        }
      }
      merged.sort((left, right) => {
        const leftName = left.local?.name || left.manifest?.name || left.id;
        const rightName = right.local?.name || right.manifest?.name || right.id;
        return leftName.localeCompare(rightName, 'zh-CN');
      });
      setEntries(merged);

      const knownArtifactIds = new Set(merged.map((entry) => entry.artifactId));
      setAvailable(manifests.filter((manifest) => !knownArtifactIds.has(manifest.id)));

      const health: Record<string, BotHealth> = {};
      await Promise.all(
        merged
          .filter((e) => e.configured)
          .map(async (e) => {
            const id = e.configured!.id;
            health[id] = await api.botHealth(id).catch(() => 'stopped' as BotHealth);
          }),
      );
      setHealthMap(health);
    } catch (err) {
      console.error('加载 bot 列表失败:', err);
    } finally {
      setIsLoading(false);
    }
  }, [showError]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void api
      .onBotInstallProgress(({ downloaded, total }) => {
        if (disposed) return;
        setInstallProgress(total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : null);
      })
      .then((stopListening) => {
        if (disposed) stopListening();
        else unlisten = stopListening;
      })
      .catch((err) => console.error('监听 Bot 安装进度失败:', err));

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  const handleInstall = async (
    manifest: BotManifest,
    destBotId: string,
    configured: BotConfig | null,
  ) => {
    setInstallingBotId(destBotId);
    setInstallProgress(null);
    try {
      await api.botInstall(manifest.id, destBotId);
      const installed = (await api.botScanLocal()).find((item) => item.id === destBotId);
      if (!installed) throw new Error('安装完成后未发现 Bot 程序');

      showSuccess('安装完成', `“${manifest.name || destBotId}”已安装`);
      await load();
      setFormArtifact(installed);
      setFormBot(configured);
    } catch (err) {
      showError('安装失败', String(err));
    } finally {
      setInstallingBotId(null);
      setInstallProgress(null);
    }
  };

  const handleUpgrade = async (
    bot: BotConfig,
    name: string,
    localVersion: string,
    manifest: BotManifest,
  ) => {
    if (
      !confirm(
        `“${name}”将从 ${localVersion} 更新到 ${manifest.version}。更新期间会停止 Bot，完成后恢复当前状态。`,
      )
    )
      return;

    setUpgrading((prev) => ({ ...prev, [bot.id]: true }));
    try {
      const result = await api.botUpgrade(bot.id);
      showSuccess('更新完成', `“${name}”已更新到 ${manifest.version}。${result}`);
    } catch (err) {
      showError('更新失败', String(err));
    } finally {
      await load();
      setUpgrading((prev) => ({ ...prev, [bot.id]: false }));
    }
  };

  const handleStart = async (bot: BotConfig, name: string, supportsMcp: boolean) => {
    let started = false;
    try {
      await api.botStart(bot.id);
      started = true;
      if (supportsMcp) await api.botRegisterMcp(bot.id);
      showSuccess('已启动', `“${name}”已启动`);
      await load();
    } catch (err) {
      let detail = String(err);
      if (started) {
        try {
          await api.botStop(bot.id);
          detail = `${detail}；Bot 已自动停止`;
        } catch (stopErr) {
          detail = `${detail}；自动停止失败：${String(stopErr)}`;
        }
      }
      showError('启动失败', detail);
      await load();
    }
  };

  const handleStop = async (bot: BotConfig, name: string) => {
    try {
      await api.botStop(bot.id);
      showSuccess('已停止', `“${name}”已停止`);
      load();
    } catch (err) {
      showError('停止失败', String(err));
    }
  };

  const handleRemove = async (bot: BotConfig, name: string) => {
    if (!confirm(`确定删除“${name}”的配置？已安装的 Bot 程序会保留。`)) return;
    try {
      await api.botRemove(bot.id);
      showSuccess('配置已删除', `“${name}”的程序仍保留在本机，可重新配置`);
      load();
    } catch (err) {
      showError('删除失败', String(err));
    }
  };

  const openConfigure = (entry: BotEntry) => {
    if (!entry.local) return;
    setFormArtifact(entry.local);
    setFormBot(entry.configured);
  };

  const installButtonLabel = (botId: string) => {
    if (installingBotId !== botId) return '安装';
    return installProgress === null ? '安装中' : `${installProgress}%`;
  };

  const healthBadge = (id: string, configured: boolean) => {
    if (!configured) return <Badge variant="outline">待配置</Badge>;
    const h = healthMap[id];
    if (!h) return <Badge variant="secondary">未知</Badge>;
    if (h === 'running') return <Badge className="bg-green-600">运行中</Badge>;
    if (h === 'stopped') return <Badge variant="secondary">已停止</Badge>;
    if (h === 'missing_artifact') return <Badge variant="outline">未安装</Badge>;
    return <Badge variant="destructive">错误</Badge>;
  };

  return (
    <div className="p-4 space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-medium">移动端控制</h3>
        <Button
          size="sm"
          variant="outline"
          className="h-7 gap-1 px-2 text-xs [&_svg]:size-3.5"
          onClick={() => void load()}
          disabled={isLoading}
          title="刷新"
          aria-label="刷新 Bot 列表"
        >
          <RefreshCw className={isLoading ? 'animate-spin' : ''} />
          刷新
        </Button>
      </div>

      {onlineError && (
        <div
          role="alert"
          className="flex items-start gap-2 border border-destructive/40 bg-destructive/5 px-3 py-2 text-sm text-destructive"
        >
          <TriangleAlert className="mt-0.5 h-4 w-4 shrink-0" />
          <div className="min-w-0">
            <div className="font-medium">线上 Bot 加载失败</div>
            <div className="mt-0.5 break-words text-xs opacity-80">{onlineError}</div>
          </div>
        </div>
      )}

      {isLoading && entries.length === 0 && available.length === 0 ? (
        <div className="text-center text-muted-foreground py-8">加载中...</div>
      ) : (
        <div className="space-y-5">
          {entries.length > 0 && (
            <section className="space-y-2">
              <h4 className="text-sm font-medium">已安装</h4>
              {entries.map((entry) => {
                const bot = entry.configured;
                const displayName = entry.local?.name || entry.manifest?.name || entry.id;
                const version = entry.local?.version || entry.manifest?.version;
                const isRunning = bot && healthMap[bot.id] === 'running';
                const updateManifest =
                  entry.local &&
                  entry.manifest &&
                  isVersionNewer(entry.manifest.version, entry.local.version)
                    ? entry.manifest
                    : null;
                const description = entry.local
                  ? bot
                    ? `创建于 ${bot.created_at}`
                    : entry.manifest?.description || '尚未配置'
                  : entry.manifest?.description || '本地程序缺失';

                return (
                  <Card key={entry.id}>
                    <CardContent className="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-x-3 gap-y-2 py-3 sm:grid-cols-[auto_minmax(0,1fr)_auto]">
                      <BotIcon className="h-5 w-5 shrink-0 text-muted-foreground" />
                      <div className="min-w-0 flex-1">
                        <div className="flex flex-wrap items-center gap-2">
                          <span className="truncate font-medium">{displayName}</span>
                          <Badge variant="outline">{entry.artifactId}</Badge>
                          {version && (
                            <span className="text-xs text-muted-foreground">v{version}</span>
                          )}
                          {healthBadge(bot?.id ?? entry.id, !!bot)}
                        </div>
                        <div className="mt-0.5 truncate text-xs text-muted-foreground" title={description}>
                          {description}
                        </div>
                      </div>
                      <div className="col-span-2 flex flex-wrap items-center justify-end gap-1 sm:col-span-1">
                        {!bot && entry.local && (
                          <Button
                            size="sm"
                            variant="outline"
                            className="h-7 gap-1 px-2 text-xs [&_svg]:size-3.5"
                            onClick={() => openConfigure(entry)}
                          >
                            <SettingsIcon />
                            配置
                          </Button>
                        )}
                        {bot && !entry.local && entry.manifest && (
                          <Button
                            size="sm"
                            variant="outline"
                            className="h-7 min-w-[72px] gap-1 px-2 text-xs [&_svg]:size-3.5"
                            onClick={() => void handleInstall(entry.manifest!, bot.id, bot)}
                            disabled={installingBotId !== null}
                          >
                            {installingBotId === bot.id ? (
                              <Loader2 className="animate-spin" />
                            ) : (
                              <Download />
                            )}
                            {installButtonLabel(bot.id)}
                          </Button>
                        )}
                        {bot && entry.local && (
                          <>
                            {isRunning ? (
                              <Button
                                size="sm"
                                variant="ghost"
                                className="h-7 gap-1 px-2 text-xs text-red-500 hover:bg-red-500/10 hover:text-red-400 [&_svg]:size-3.5"
                                onClick={() => handleStop(bot, displayName)}
                                title="停止"
                                aria-label={`停止 ${displayName} 并取消自动运行`}
                              >
                                <Square className="fill-current" />
                                停止
                              </Button>
                            ) : (
                              <Button
                                size="sm"
                                variant="ghost"
                                className="h-7 gap-1 px-2 text-xs text-emerald-500 hover:bg-emerald-500/10 hover:text-emerald-400 [&_svg]:size-3.5"
                                onClick={() =>
                                  handleStart(bot, displayName, entry.local!.supports_mcp)
                                }
                                title="启动"
                                aria-label={`启动 ${displayName} 并设为自动运行`}
                              >
                                <Play className="fill-current" />
                                启动
                              </Button>
                            )}
                            {updateManifest && (
                              <Button
                                size="sm"
                                className="h-7 min-w-[68px] gap-1 px-2 text-xs [&_svg]:size-3.5"
                                onClick={() =>
                                  handleUpgrade(
                                    bot,
                                    displayName,
                                    entry.local!.version,
                                    updateManifest,
                                  )
                                }
                                disabled={upgrading[bot.id]}
                                title={`更新到 v${updateManifest.version}`}
                                aria-label={`更新 ${displayName} 到 ${updateManifest.version}`}
                              >
                                {upgrading[bot.id] ? (
                                  <Loader2 className="animate-spin" />
                                ) : (
                                  <ArrowUpCircle />
                                )}
                                {upgrading[bot.id] ? '更新中' : '更新'}
                              </Button>
                            )}
                            <Button
                              size="sm"
                              variant="ghost"
                              className="h-7 gap-1 px-2 text-xs [&_svg]:size-3.5"
                              onClick={() => setLogBotId(bot.id)}
                              title="查看日志"
                              aria-label={`查看 ${displayName} 日志`}
                            >
                              <FileText />
                              日志
                            </Button>
                            {entry.local.supports_mcp && (
                              <Button
                                size="sm"
                                variant="ghost"
                                className="h-7 gap-1 px-2 text-xs [&_svg]:size-3.5"
                                onClick={() =>
                                  setPushTargetBot({ id: bot.id, name: displayName })
                                }
                                title="管理推送授权"
                                aria-label={`管理 ${displayName} 推送授权`}
                              >
                                <MessageSquareText />
                                推送授权
                              </Button>
                            )}
                            <Button
                              size="sm"
                              variant="ghost"
                              className="h-7 gap-1 px-2 text-xs [&_svg]:size-3.5"
                              onClick={() => openConfigure(entry)}
                              title="编辑配置"
                              aria-label={`编辑 ${displayName} 配置`}
                            >
                              <SettingsIcon />
                              配置
                            </Button>
                          </>
                        )}
                        {bot && (
                          <Button
                            size="sm"
                            variant="ghost"
                            className="h-7 gap-1 px-2 text-xs hover:bg-destructive/20 hover:text-destructive [&_svg]:size-3.5"
                            onClick={() => handleRemove(bot, displayName)}
                            title="删除配置"
                            aria-label={`删除 ${displayName} 配置`}
                          >
                            <Trash2 />
                            删除
                          </Button>
                        )}
                      </div>
                    </CardContent>
                  </Card>
                );
              })}
            </section>
          )}

          {available.length > 0 && (
            <section className="space-y-2">
              <h4 className="text-sm font-medium">可安装</h4>
              {available.map((manifest) => (
                <Card key={manifest.id}>
                  <CardContent className="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-x-3 gap-y-2 py-3 sm:grid-cols-[auto_minmax(0,1fr)_auto]">
                    <BotIcon className="h-5 w-5 shrink-0 text-muted-foreground" />
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="truncate font-medium">{manifest.name || manifest.id}</span>
                        <Badge variant="outline">{manifest.id}</Badge>
                        <span className="text-xs text-muted-foreground">v{manifest.version}</span>
                        <Badge variant="secondary">未安装</Badge>
                      </div>
                      {manifest.description && (
                        <div
                          className="mt-0.5 truncate text-xs text-muted-foreground"
                          title={manifest.description}
                        >
                          {manifest.description}
                        </div>
                      )}
                    </div>
                    <div className="col-span-2 flex justify-end sm:col-span-1">
                      <Button
                        size="sm"
                        className="h-7 min-w-[72px] gap-1 px-2 text-xs [&_svg]:size-3.5"
                        onClick={() => void handleInstall(manifest, manifest.id, null)}
                        disabled={installingBotId !== null}
                      >
                        {installingBotId === manifest.id ? (
                          <Loader2 className="animate-spin" />
                        ) : (
                          <Download />
                        )}
                        {installButtonLabel(manifest.id)}
                      </Button>
                    </div>
                  </CardContent>
                </Card>
              ))}
            </section>
          )}

          {entries.length === 0 && available.length === 0 && (
            <Card>
              <CardContent className="py-3 text-sm text-muted-foreground">
                {onlineError ? '暂无已安装 Bot' : '暂无可用 Bot'}
              </CardContent>
            </Card>
          )}
        </div>
      )}

      {/* 配置/编辑表单 */}
      {formArtifact && (
        <BotFormDialog
          bot={formBot}
          artifactId={formArtifact.artifact_id}
          artifactName={formArtifact.name || formArtifact.id}
          suggestedId={formArtifact.id}
          onClose={() => {
            setFormArtifact(null);
            setFormBot(null);
          }}
          onSaved={load}
        />
      )}

      {logBotId && <BotLogDialog botId={logBotId} onClose={() => setLogBotId(null)} />}

      {pushTargetBot && (
        <BotPushTargetsDialog
          botId={pushTargetBot.id}
          botName={pushTargetBot.name}
          onClose={() => setPushTargetBot(null)}
        />
      )}
    </div>
  );
}
