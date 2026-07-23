import { useEffect, useState } from 'react';
import {
  api,
  type BotConfig,
  type BotHealth,
  type LocalArtifact,
} from '../../api/tauri';
import { Button } from '../ui/button';
import { Badge } from '../ui/badge';
import { Card, CardContent } from '../ui/card';
import {
  ArrowUpCircle,
  Bot as BotIcon,
  FileText,
  Play,
  RefreshCw,
  Settings as SettingsIcon,
  Square,
  Trash2,
} from 'lucide-react';
import { useToast } from '../Toast';
import { BotFormDialog } from './BotFormDialog';
import { BotLogDialog } from './BotLogDialog';

/** 列表条目——本地制品 + 可选的已配置 BotConfig。 */
interface BotEntry {
  /** 本地制品信息（来自扫描）。 */
  local: LocalArtifact;
  /** 已配置的 bot（来自 bots.json），null 表示未配置。 */
  configured: BotConfig | null;
}

/** bot 列表面板——本地制品直接展示，按配置状态显示不同操作。 */
export function BotPanel() {
  const { showSuccess, showError } = useToast();
  const [entries, setEntries] = useState<BotEntry[]>([]);
  const [healthMap, setHealthMap] = useState<Record<string, BotHealth>>({});
  const [isLoading, setIsLoading] = useState(false);

  // 编辑/配置表单。
  // bot 非空 → 编辑已有配置；bot 为空 → 配置未配置的本地制品。
  const [formArtifact, setFormArtifact] = useState<LocalArtifact | null>(null);
  const [formBot, setFormBot] = useState<BotConfig | null>(null);
  const [logBotId, setLogBotId] = useState<string | null>(null);

  // 检查更新中状态。
  const [checking, setChecking] = useState<Record<string, boolean>>({});

  const load = async () => {
    setIsLoading(true);
    try {
      const [local, bots] = await Promise.all([api.botScanLocal(), api.botList()]);
      // 合并：本地制品 + 已注册但制品还在的 bot。
      const botById = new Map(bots.map((b) => [b.id, b]));
      const merged: BotEntry[] = local.map((l) => ({
        local: l,
        configured: botById.get(l.id) ?? null,
      }));
      // 已注册但本地扫描未覆盖的 bot（制品可能被删）也展示。
      const localIds = new Set(local.map((l) => l.id));
      for (const bot of bots) {
        if (!localIds.has(bot.id)) {
          merged.push({
            local: {
              id: bot.id,
              name: bot.id,
              artifact_id: bot.artifact_id,
              version: '',
              config_schema: [],
            },
            configured: bot,
          });
        }
      }
      setEntries(merged);

      // 异步刷新已配置 bot 的健康状态。
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
  };

  useEffect(() => {
    load();
  }, []);

  const handleCheckUpdate = async (bot: BotConfig, name: string) => {
    setChecking((prev) => ({ ...prev, [bot.id]: true }));
    try {
      const manifest = await api.botCheckUpdate(bot.artifact_id);
      if (!manifest) {
        showSuccess('已是最新', `“${name}”已是最新版本`);
        return;
      }
      if (!confirm(`“${name}”发现新版本 ${manifest.version}，是否升级？升级会先停止 Bot。`))
        return;
      await api.botUpgrade(bot.id);
      showSuccess('升级完成', `“${name}”已升级到 ${manifest.version}，请重新启动`);
      load();
    } catch (err) {
      showError('检查更新失败', String(err));
    } finally {
      setChecking((prev) => ({ ...prev, [bot.id]: false }));
    }
  };

  const handleStart = async (bot: BotConfig, name: string) => {
    try {
      await api.botStart(bot.id);
      showSuccess('已启动', `“${name}”已启动`);
      load();
    } catch (err) {
      showError('启动失败', String(err));
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
    setFormArtifact(entry.local);
    setFormBot(entry.configured);
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
        <Button size="sm" variant="outline" onClick={load}>
          <RefreshCw className="w-4 h-4 mr-2" />
          刷新
        </Button>
      </div>

      {isLoading ? (
        <div className="text-center text-muted-foreground py-8">加载中...</div>
      ) : entries.length === 0 ? (
        <Card>
          <CardContent className="text-sm text-muted-foreground py-3">
            暂无 bot 制品。请将 bot 二进制放入 ~/.tiangong/bots/&lt;名称&gt;/ 目录。
          </CardContent>
        </Card>
      ) : (
        <div className="space-y-2">
          {entries.map((entry) => {
            const bot = entry.configured;
            const displayName = entry.local.name || entry.local.id;
            const isRunning = bot && healthMap[bot.id] === 'running';
            return (
              <Card key={entry.local.id}>
                <CardContent className="grid grid-cols-[auto_minmax(0,1fr)] items-center gap-x-3 gap-y-2 py-3 sm:grid-cols-[auto_minmax(0,1fr)_auto]">
                  <BotIcon className="w-5 h-5 text-muted-foreground shrink-0" />
                  <div className="flex-1 min-w-0">
                    <div className="flex flex-wrap items-center gap-2">
                      <span className="font-medium truncate">{displayName}</span>
                      <Badge variant="outline">{entry.local.artifact_id}</Badge>
                      {entry.local.version && (
                        <span className="text-xs text-muted-foreground">v{entry.local.version}</span>
                      )}
                      {healthBadge(bot?.id ?? entry.local.id, !!bot)}
                    </div>
                    <div className="text-xs text-muted-foreground mt-0.5">
                      {bot ? `创建于 ${bot.created_at}` : '本地制品，尚未配置'}
                    </div>
                  </div>
                  <div className="col-span-2 flex shrink-0 items-center justify-end gap-1 sm:col-span-1">
                    {/* 未配置 → 配置按钮 */}
                    {!bot && (
                      <Button
                        size="sm"
                        variant="outline"
                        onClick={() => openConfigure(entry)}
                        title="配置"
                      >
                        <SettingsIcon className="w-4 h-4 mr-1" />
                        配置
                      </Button>
                    )}
                    {/* 已配置 → 启停 + 检查更新 + 配置 + 删除 */}
                    {bot && (
                      <>
                        {isRunning ? (
                          <Button
                            size="icon"
                            variant="ghost"
                            className="h-5 w-5 rounded-none p-0 text-red-500 hover:bg-transparent hover:text-red-400"
                            onClick={() => handleStop(bot, displayName)}
                            title="停止"
                            aria-label={`停止 ${displayName} 并取消自动运行`}
                          >
                            <Square className="!h-5 !w-5 fill-current" />
                          </Button>
                        ) : (
                          <Button
                            size="icon"
                            variant="ghost"
                            className="h-5 w-5 rounded-none p-0 text-emerald-500 hover:bg-transparent hover:text-emerald-400"
                            onClick={() => handleStart(bot, displayName)}
                            title="启动"
                            aria-label={`启动 ${displayName} 并设为自动运行`}
                          >
                            <Play className="!h-5 !w-5 fill-current" />
                          </Button>
                        )}
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() => handleCheckUpdate(bot, displayName)}
                          disabled={checking[bot.id]}
                          title="检查更新"
                        >
                          <ArrowUpCircle className="w-4 h-4" />
                        </Button>
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() => setLogBotId(bot.id)}
                          title="查看日志"
                          aria-label={`查看 ${displayName} 日志`}
                        >
                          <FileText className="w-4 h-4" />
                        </Button>
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() => openConfigure(entry)}
                          title="编辑配置"
                        >
                          <SettingsIcon className="w-4 h-4" />
                        </Button>
                        <Button
                          size="sm"
                          variant="ghost"
                          className="hover:bg-destructive/20 hover:text-destructive"
                          onClick={() => handleRemove(bot, displayName)}
                          title="删除配置"
                          aria-label={`删除 ${displayName} 配置`}
                        >
                          <Trash2 className="w-4 h-4" />
                        </Button>
                      </>
                    )}
                  </div>
                </CardContent>
              </Card>
            );
          })}
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
    </div>
  );
}
