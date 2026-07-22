import { useEffect, useState } from 'react';
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
import { Switch } from '../ui/switch';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '../ui/dialog';
import { Plus, Trash2, RefreshCw, Play, Square, Download, Bot as BotIcon, ArrowUpCircle } from 'lucide-react';
import { useToast } from '../Toast';
import { BotFormDialog } from './BotFormDialog';

/** bot 列表面板——注册、安装制品、启停、删除。 */
export function BotPanel() {
  const { showSuccess, showError } = useToast();
  const [bots, setBots] = useState<BotConfig[]>([]);
  const [healthMap, setHealthMap] = useState<Record<string, BotHealth>>({});
  const [available, setAvailable] = useState<BotManifest[]>([]);
  const [localArtifacts, setLocalArtifacts] = useState<LocalArtifact[]>([]);
  const [isLoading, setIsLoading] = useState(false);

  // 新增表单：先选制品，再打开 BotFormDialog。
  const [pickOpen, setPickOpen] = useState(false);
  const [pickedArtifactId, setPickedArtifactId] = useState<string | null>(null);

  // 编辑表单。
  const [editing, setEditing] = useState<BotConfig | null>(null);

  // 安装中状态：botId → boolean
  const [installing, setInstalling] = useState<Record<string, boolean>>({});
  // 检查更新中状态：botId → boolean
  const [checking, setChecking] = useState<Record<string, boolean>>({});

  const load = async () => {
    setIsLoading(true);
    try {
      const list = await api.botList();
      setBots(list);
      // 异步刷新健康状态。
      const health: Record<string, BotHealth> = {};
      await Promise.all(
        list.map(async (b) => {
          health[b.id] = await api.botHealth(b.id).catch(() => 'stopped' as BotHealth);
        }),
      );
      setHealthMap(health);
    } catch (err) {
      console.error('加载 bot 列表失败:', err);
    } finally {
      setIsLoading(false);
    }
  };

  const refreshAvailable = async () => {
    try {
      const index = await api.botAvailable();
      setAvailable(index.bots);
    } catch (err) {
      console.warn('拉取可安装 bot 列表失败:', err);
    }
    // 同时扫描本地已安装的制品（不依赖线上 index）。
    try {
      const local = await api.botScanLocal();
      setLocalArtifacts(local);
    } catch (err) {
      console.warn('扫描本地 bot 制品失败:', err);
    }
  };

  // 合并本地 + 线上的可选制品（去重，本地优先）。
  const pickableArtifacts = (() => {
    const map = new Map<string, { id: string; name: string; source: 'local' | 'remote' }>();
    for (const a of localArtifacts) {
      map.set(a.artifact_id, { id: a.artifact_id, name: a.id, source: 'local' });
    }
    for (const m of available) {
      if (!map.has(m.id)) {
        map.set(m.id, { id: m.id, name: m.name, source: 'remote' });
      }
    }
    return Array.from(map.values());
  })();

  useEffect(() => {
    load();
    refreshAvailable();
  }, []);

  const handleToggleEnabled = async (id: string, enabled: boolean) => {
    try {
      await api.botSetEnabled(id, enabled);
      showSuccess('状态更新', `bot 已${enabled ? '启用' : '禁用'}`);
      load();
    } catch (err) {
      showError('操作失败', String(err));
    }
  };

  const handleInstall = async (bot: BotConfig) => {
    setInstalling((prev) => ({ ...prev, [bot.id]: true }));
    try {
      // 按 artifact_id 在 available 列表找制品。
      const manifest = available.find((m) => m.id === bot.artifact_id);
      if (!manifest) {
        showError('制品不可用', `bots-index 中未找到 ${bot.artifact_id} 的制品，请先发布`);
        return;
      }
      await api.botInstall(manifest.id, bot.id);
      showSuccess('安装完成', `bot "${bot.id}" 制品已安装`);
      load();
    } catch (err) {
      showError('安装失败', String(err));
    } finally {
      setInstalling((prev) => ({ ...prev, [bot.id]: false }));
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
    if (!confirm(`确定删除 bot "${bot.id}"？`)) return;
    try {
      await api.botRemove(bot.id);
      showSuccess('已删除', `bot "${bot.id}" 已删除`);
      load();
    } catch (err) {
      showError('删除失败', String(err));
    }
  };

  const healthBadge = (id: string) => {
    const h = healthMap[id];
    if (!h) return <Badge variant="secondary">未知</Badge>;
    if (h === 'running') return <Badge className="bg-green-600">运行中</Badge>;
    if (h === 'stopped') return <Badge variant="secondary">已停止</Badge>;
    if (h === 'missing_artifact') return <Badge variant="outline">未安装</Badge>;
    return <Badge variant="destructive">错误</Badge>;
  };

  const isInstalled = (id: string) => {
    const h = healthMap[id];
    return h === 'running' || h === 'stopped';
  };

  return (
    <div className="p-4 space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-medium">移动端控制</h3>
        <div className="flex items-center gap-2">
          <Button size="sm" variant="outline" onClick={refreshAvailable}>
            <RefreshCw className="w-4 h-4 mr-2" />
            刷新
          </Button>
          <Button
            size="sm"
            onClick={() => setPickOpen(true)}
            disabled={pickableArtifacts.length === 0}
          >
            <Plus className="w-4 h-4 mr-2" />
            添加
          </Button>
        </div>
      </div>

      {pickableArtifacts.length === 0 && (
        <Card>
          <CardContent className="text-sm text-muted-foreground py-3">
            暂无可用 bot 制品。请先将 bot 二进制放入 ~/.tiangong/bots/&lt;名称&gt;/，
            或发布包含 bots-index.json 的 Release。
          </CardContent>
        </Card>
      )}

      {isLoading ? (
        <div className="text-center text-muted-foreground py-8">加载中...</div>
      ) : bots.length === 0 ? (
        <div className="text-center text-muted-foreground py-8">暂无 bot</div>
      ) : (
        <div className="space-y-2">
          {bots.map((bot) => (
            <Card key={bot.id}>
              <CardContent className="flex items-center gap-3 py-3">
                <BotIcon className="w-5 h-5 text-muted-foreground shrink-0" />
                <div className="flex-1 min-w-0">
                  <div className="flex items-center gap-2">
                    <span className="font-medium truncate">{bot.id}</span>
                    <Badge variant="outline">{bot.artifact_id}</Badge>
                    {healthBadge(bot.id)}
                  </div>
                  <div className="text-xs text-muted-foreground mt-0.5">
                    创建于 {bot.created_at}
                  </div>
                </div>
                <div className="flex items-center gap-1 shrink-0">
                  {!isInstalled(bot.id) && (
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => handleInstall(bot)}
                      disabled={installing[bot.id]}
                      title="下载制品"
                    >
                      <Download className="w-4 h-4" />
                    </Button>
                  )}
                  {isInstalled(bot.id) && (
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => handleCheckUpdate(bot)}
                      disabled={checking[bot.id]}
                      title="检查更新"
                    >
                      <ArrowUpCircle className="w-4 h-4" />
                    </Button>
                  )}
                  {healthMap[bot.id] === 'running' ? (
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => handleStop(bot)}
                      title="停止"
                    >
                      <Square className="w-4 h-4" />
                    </Button>
                  ) : isInstalled(bot.id) ? (
                    <Button
                      size="sm"
                      variant="ghost"
                      onClick={() => handleStart(bot)}
                      title="启动"
                    >
                      <Play className="w-4 h-4" />
                    </Button>
                  ) : null}
                  <Switch
                    checked={bot.enabled}
                    onCheckedChange={(c) => handleToggleEnabled(bot.id, c)}
                  />
                  <Button
                    size="sm"
                    variant="ghost"
                    className="hover:bg-destructive/20 hover:text-destructive"
                    onClick={() => handleRemove(bot)}
                    title="删除"
                  >
                    <Trash2 className="w-4 h-4" />
                  </Button>
                </div>
              </CardContent>
            </Card>
          ))}
        </div>
      )}

      {/* 选平台类型 */}
      <Dialog open={pickOpen} onOpenChange={setPickOpen}>
        <DialogContent className="max-w-sm">
          <DialogHeader>
            <DialogTitle>选择平台</DialogTitle>
          </DialogHeader>
          <div className="space-y-2 py-2">
            {pickableArtifacts.map((a) => (
              <Button
                key={a.id}
                variant="outline"
                className="w-full justify-start"
                onClick={() => {
                  setPickedArtifactId(a.id);
                  setPickOpen(false);
                }}
              >
                <span className="font-medium">{a.name}</span>
                <span className="text-xs text-muted-foreground ml-2">{a.id}</span>
                {a.source === 'local' && (
                  <Badge variant="secondary" className="ml-auto">本地</Badge>
                )}
              </Button>
            ))}
          </div>
        </DialogContent>
      </Dialog>

      {/* 新增/编辑表单 */}
      {(pickedArtifactId || editing) && (
        <BotFormDialog
          bot={editing}
          artifactId={editing ? editing.artifact_id : (pickedArtifactId as string)}
          onClose={() => {
            setPickedArtifactId(null);
            setEditing(null);
          }}
          onSaved={load}
        />
      )}
    </div>
  );
}
