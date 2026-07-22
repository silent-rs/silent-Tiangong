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
import { Switch } from '../ui/switch';
import { Trash2, RefreshCw, Play, Square, Settings as SettingsIcon, Bot as BotIcon, ArrowUpCircle } from 'lucide-react';
import { useToast } from '../Toast';
import { BotFormDialog } from './BotFormDialog';

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

  const handleToggleEnabled = async (bot: BotConfig, enabled: boolean) => {
    try {
      await api.botSetEnabled(bot.id, enabled);
      showSuccess('状态更新', `bot 已${enabled ? '启用' : '禁用'}`);
      load();
    } catch (err) {
      showError('操作失败', String(err));
    }
  };

  const handleCheckUpdate = async (bot: BotConfig) => {
    setChecking((prev) => ({ ...prev, [bot.id]: true }));
    try {
      const manifest = await api.botCheckUpdate(bot.artifact_id);
      if (!manifest) {
        showSuccess('已是最新', `bot "${bot.id}" 已是最新版本`);
        return;
      }
      if (!confirm(`发现新版本 ${manifest.version}，是否升级？升级会先停止 bot。`)) return;
      await api.botUpgrade(bot.id);
      showSuccess('升级完成', `bot "${bot.id}" 已升级到 ${manifest.version}，请重新启动`);
      load();
    } catch (err) {
      showError('检查更新失败', String(err));
    } finally {
      setChecking((prev) => ({ ...prev, [bot.id]: false }));
    }
  };

  const handleStart = async (bot: BotConfig) => {
    try {
      await api.botStart(bot.id);
      showSuccess('已启动', `bot "${bot.id}" 已启动`);
      load();
    } catch (err) {
      showError('启动失败', String(err));
    }
  };

  const handleStop = async (bot: BotConfig) => {
    try {
      await api.botStop(bot.id);
      showSuccess('已停止', `bot "${bot.id}" 已停止`);
      load();
    } catch (err) {
      showError('停止失败', String(err));
    }
  };

  const handleRemove = async (bot: BotConfig) => {
    if (!confirm(`确定删除 bot "${bot.id}" 的配置？`)) return;
    try {
      await api.botRemove(bot.id);
      showSuccess('已删除', `bot "${bot.id}" 配置已删除`);
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
            const isRunning = bot && healthMap[bot.id] === 'running';
            return (
              <Card key={entry.local.id}>
                <CardContent className="flex items-center gap-3 py-3">
                  <BotIcon className="w-5 h-5 text-muted-foreground shrink-0" />
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <span className="font-medium truncate">{entry.local.id}</span>
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
                  <div className="flex items-center gap-1 shrink-0">
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
                    {/* 已配置 → 启停 + 检查更新 + 开关 + 删除 */}
                    {bot && (
                      <>
                        {isRunning ? (
                          <Button
                            size="sm"
                            variant="ghost"
                            onClick={() => handleStop(bot)}
                            title="停止"
                          >
                            <Square className="w-4 h-4" />
                          </Button>
                        ) : (
                          <Button
                            size="sm"
                            variant="ghost"
                            onClick={() => handleStart(bot)}
                            title="启动"
                          >
                            <Play className="w-4 h-4" />
                          </Button>
                        )}
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() => handleCheckUpdate(bot)}
                          disabled={checking[bot.id]}
                          title="检查更新"
                        >
                          <ArrowUpCircle className="w-4 h-4" />
                        </Button>
                        <Switch
                          checked={bot.enabled}
                          onCheckedChange={(c) => handleToggleEnabled(bot, c)}
                        />
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
                          onClick={() => handleRemove(bot)}
                          title="删除配置"
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
          suggestedId={formArtifact.id}
          onClose={() => {
            setFormArtifact(null);
            setFormBot(null);
          }}
          onSaved={load}
        />
      )}
    </div>
  );
}
