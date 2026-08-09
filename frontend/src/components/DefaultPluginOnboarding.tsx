import { useEffect, useMemo, useState } from 'react';
import { CheckCircle2, Download, Loader2, XCircle } from 'lucide-react';
import { api, type AvailablePlugin } from '@/api/tauri';
import { Badge } from './ui/badge';
import { Button } from './ui/button';
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from './ui/dialog';
import { Separator } from './ui/separator';
import { Tabs, TabsContent, TabsList, TabsTrigger } from './ui/tabs';
import { useToast } from './Toast';

/** 分类标识到展示名称的映射。 */
const CATEGORY_LABELS: Record<string, string> = {
  daily: '日常工作',
  coding: '编程开发',
};
/** 分组展示顺序。 */
const CATEGORY_ORDER = ['daily', 'coding'];

type InstallState = 'pending' | 'installing' | 'installed' | 'failed';

type ItemState = {
  plugin: AvailablePlugin;
  state: InstallState;
  error?: string;
  progress?: number | null;
};

type Props = {
  /** 缺失的默认插件列表；为 null 时不显示对话框。 */
  missing: AvailablePlugin[] | null;
  /** 关闭/打开变化。关闭后父组件应清空 missing。 */
  onOpenChange: (open: boolean) => void;
  /** 引导结束后回调（用于刷新插件贡献项等全局状态）。 */
  onComplete: () => void;
};

export function DefaultPluginOnboarding({ missing, onOpenChange, onComplete }: Props) {
  const open = missing !== null && missing.length > 0;
  const [items, setItems] = useState<ItemState[]>([]);
  const [batchRunning, setBatchRunning] = useState(false);
  const { showSuccess, showError } = useToast();

  // 列表变化时重置每项状态为待安装。
  useEffect(() => {
    setItems(
      (missing ?? []).map((plugin) => ({
        plugin,
        state: plugin.installed_version ? 'installed' : 'pending',
      })),
    );
    setBatchRunning(false);
  }, [missing]);

  // 可安装项 = 待安装 + 之前失败可重试。
  const installableCount = useMemo(
    () => items.filter((item) => item.state === 'pending' || item.state === 'failed').length,
    [items],
  );
  const hasPending = installableCount > 0;

  // 按场景分类分组，一个插件同时属于多个分类时在每个分组各出现一次。
  const grouped = useMemo(() => {
    const byCategory = new Map<string, ItemState[]>();
    for (const category of CATEGORY_ORDER) byCategory.set(category, []);
    for (const item of items) {
      for (const category of item.plugin.categories.length > 0
        ? item.plugin.categories
        : ['daily']) {
        if (byCategory.has(category)) byCategory.get(category)!.push(item);
      }
    }
    return CATEGORY_ORDER.map((category) => ({
      category,
      label: CATEGORY_LABELS[category] ?? category,
      items: byCategory.get(category) ?? [],
    })).filter((group) => group.items.length > 0);
  }, [items]);

  const updateItem = (id: string, patch: Partial<ItemState>) => {
    setItems((prev) => prev.map((item) => (item.plugin.id === id ? { ...item, ...patch } : item)));
  };

  const installOne = async (plugin: AvailablePlugin): Promise<boolean> => {
    updateItem(plugin.id, { state: 'installing', error: undefined, progress: 0 });
    try {
      await api.installPlugin(plugin.id);
      updateItem(plugin.id, { state: 'installed', progress: null });
      return true;
    } catch (error) {
      updateItem(plugin.id, { state: 'failed', error: String(error), progress: null });
      return false;
    }
  };

  // 监听后端推送的下载进度事件，更新对应插件的下载百分比。
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    void api
      .onPluginInstallProgress(({ plugin_id, downloaded, total }) => {
        if (disposed) return;
        const percent = total > 0 ? Math.min(100, Math.round((downloaded / total) * 100)) : 0;
        updateItem(plugin_id, { progress: percent });
      })
      .then((stop) => {
        if (disposed) {
          stop();
        } else {
          unlisten = stop;
        }
      });
    return () => {
      disposed = true;
      unlisten?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 一键串行安装：逐个下载，失败不阻断后续。
  const installAll = async () => {
    if (batchRunning) return;
    const targets = items.filter((item) => item.state === 'pending' || item.state === 'failed');
    if (targets.length === 0) return;
    setBatchRunning(true);
    let succeeded = 0;
    for (const { plugin } of targets) {
      if (await installOne(plugin)) succeeded += 1;
    }
    setBatchRunning(false);
    await api.completeFirstLaunch().catch((error) => console.warn('写入首次启动标记失败', error));
    if (succeeded === targets.length) {
      showSuccess('默认插件已全部安装', '可以开始使用天工了');
    } else if (succeeded > 0) {
      showSuccess(`已安装 ${succeeded} 个插件`, '部分插件安装失败，可稍后在插件管理中重试');
    } else {
      showError('安装失败', '可在插件管理中手动重试');
    }
    onComplete();
    onOpenChange(false);
  };

  // 跳过：直接写入完成标记并关闭，本次启动不再弹出。
  const skip = async () => {
    if (batchRunning) return;
    await api.completeFirstLaunch().catch((error) => console.warn('写入首次启动标记失败', error));
    onOpenChange(false);
  };

  return (
    <Dialog open={open} onOpenChange={(next) => (next ? null : onOpenChange(false))}>
      <DialogContent className="max-w-lg" showCloseButton={!batchRunning}>
        <DialogHeader>
          <DialogTitle>欢迎使用天工</DialogTitle>
          <DialogDescription>
            为获得基础体验，建议安装以下默认插件。它们提供系统提示词、文件操作、命令执行等基础能力。
          </DialogDescription>
        </DialogHeader>

        <Tabs defaultValue={grouped[0]?.category ?? 'daily'}>
          <TabsList className="grid w-full grid-cols-2">
            {grouped.map((group) => (
              <TabsTrigger key={group.category} value={group.category} className="py-1 text-xs">
                {group.label}
              </TabsTrigger>
            ))}
          </TabsList>

          {grouped.map((group) => (
            <TabsContent key={group.category} value={group.category} className="m-0">
              <div className="max-h-[40vh] overflow-y-auto">
                {group.items.map((item, index) => (
                  <div key={item.plugin.id}>
                    {index > 0 && <Separator />}
                    <PluginRow
                      item={item}
                      disabled={batchRunning}
                      onInstall={() => void installOne(item.plugin)}
                    />
                  </div>
                ))}
              </div>
            </TabsContent>
          ))}
        </Tabs>

        <DialogFooter className="gap-2 sm:gap-2">
          <Button variant="ghost" onClick={() => void skip()} disabled={batchRunning}>
            跳过
          </Button>
          <Button onClick={() => void installAll()} disabled={batchRunning || !hasPending}>
            {batchRunning ? (
              <>
                <Loader2 className="mr-2 h-4 w-4 animate-spin" />
                正在安装...
              </>
            ) : (
              <>
                <Download className="mr-2 h-4 w-4" />
                一键安装{hasPending ? `（${installableCount}）` : ''}
              </>
            )}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function PluginRow({
  item,
  disabled,
  onInstall,
}: {
  item: ItemState;
  disabled: boolean;
  onInstall: () => void;
}) {
  const { plugin, state, progress } = item;
  return (
    <div className="flex min-h-20 items-start gap-4 py-4">
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="break-words text-sm font-medium">{plugin.name}</span>
          <Badge variant="outline">{plugin.version}</Badge>
          {state === 'installed' && <Badge variant="secondary">已安装</Badge>}
          {state === 'failed' && <Badge variant="destructive">失败</Badge>}
        </div>
        {plugin.description && (
          <p className="mt-1.5 break-words text-xs leading-5 text-muted-foreground">
            {plugin.description}
          </p>
        )}
        {state === 'failed' && item.error && (
          <p className="mt-1 break-words text-xs text-destructive">{item.error}</p>
        )}
        {state === 'installing' && progress !== null && progress !== undefined && (
          <div className="mt-2 h-1 w-full max-w-56 overflow-hidden rounded-full bg-muted">
            <div
              className="h-full bg-primary transition-[width] duration-150"
              style={{ width: `${progress}%` }}
            />
          </div>
        )}
      </div>
      <StateIndicator state={state} disabled={disabled} onInstall={onInstall} />
    </div>
  );
}

function StateIndicator({
  state,
  disabled,
  onInstall,
}: {
  state: InstallState;
  disabled: boolean;
  onInstall: () => void;
}) {
  if (state === 'installed') {
    return <CheckCircle2 className="mt-0.5 h-5 w-5 shrink-0 text-emerald-500" />;
  }
  if (state === 'installing') {
    return <Loader2 className="mt-0.5 h-5 w-5 shrink-0 animate-spin text-muted-foreground" />;
  }
  if (state === 'failed') {
    return (
      <div className="flex shrink-0 items-center gap-1">
        <XCircle className="h-5 w-5 text-destructive" />
        <Button variant="outline" size="sm" onClick={onInstall} disabled={disabled}>
          重试
        </Button>
      </div>
    );
  }
  return (
    <Button variant="outline" size="sm" onClick={onInstall} disabled={disabled}>
      安装
    </Button>
  );
}
