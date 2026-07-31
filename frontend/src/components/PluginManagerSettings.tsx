import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  AlertCircle,
  ArrowUpCircle,
  CheckCircle2,
  Download,
  Loader2,
  RefreshCw,
  RotateCw,
  Trash2,
  Undo2,
} from 'lucide-react';
import {
  api,
  type AvailablePlugin,
  type PluginContributionEntry,
  type PluginStatus,
} from '@/api/tauri';
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
import { Switch } from './ui/switch';
import { Tabs, TabsContent, TabsList, TabsTrigger } from './ui/tabs';
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from './ui/tooltip';
import { useToast } from './Toast';

type Props = {
  onContributionsChanged: (entries: PluginContributionEntry[]) => void;
};

type Operation =
  | 'enable'
  | 'disable'
  | 'install'
  | 'reload'
  | 'upgrade'
  | 'rollback'
  | 'uninstall';

type ActiveOperation = {
  pluginId: string;
  operation: Operation;
};

const stateLabel: Record<PluginStatus['state'], string> = {
  loaded: '已加载',
  disabled: '已停用',
  degraded: '运行异常',
  error: '加载失败',
};

export function PluginManagerSettings({ onContributionsChanged }: Props) {
  const [plugins, setPlugins] = useState<PluginStatus[]>([]);
  const [available, setAvailable] = useState<AvailablePlugin[]>([]);
  const [loading, setLoading] = useState(true);
  const [catalogError, setCatalogError] = useState<string | null>(null);
  const [activeOperation, setActiveOperation] = useState<ActiveOperation | null>(null);
  const [uninstallTarget, setUninstallTarget] = useState<PluginStatus | null>(null);
  const [keepData, setKeepData] = useState(true);
  const { showError, showSuccess } = useToast();

  const refreshContributions = useCallback(async () => {
    const contributions = await api.listPluginContributions();
    onContributionsChanged(contributions.filter((entry) => entry.has_view));
  }, [onContributionsChanged]);

  const refresh = useCallback(async () => {
    setLoading(true);
    const [installedResult, availableResult] = await Promise.allSettled([
      api.listPlugins(),
      api.listAvailablePlugins(),
    ]);
    if (installedResult.status === 'fulfilled') {
      setPlugins(installedResult.value);
    } else {
      showError('读取失败', String(installedResult.reason));
    }
    if (availableResult.status === 'fulfilled') {
      setAvailable(availableResult.value);
      setCatalogError(null);
    } else {
      setCatalogError(String(availableResult.reason));
    }
    setLoading(false);
  }, [showError]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const availableById = useMemo(
    () => new Map(available.map((plugin) => [plugin.id, plugin])),
    [available],
  );

  const finishOperation = async () => {
    const [, contributionsResult] = await Promise.allSettled([
      refresh(),
      refreshContributions(),
    ]);
    if (contributionsResult.status === 'rejected') {
      showError('插件页面刷新失败', String(contributionsResult.reason));
    }
  };

  const runOperation = async (
    pluginId: string,
    operation: Operation,
    action: () => Promise<unknown>,
    successTitle: string,
    successMessage?: string,
  ) => {
    setActiveOperation({ pluginId, operation });
    try {
      await action();
      await finishOperation();
      showSuccess(successTitle, successMessage);
      return true;
    } catch (error) {
      showError(`${operationLabel(operation)}失败`, String(error));
      await refresh();
      return false;
    } finally {
      setActiveOperation(null);
    }
  };

  const toggleEnabled = async (plugin: PluginStatus, enabled: boolean) => {
    await runOperation(
      plugin.id,
      enabled ? 'enable' : 'disable',
      () => api.setPluginEnabled(plugin.id, enabled),
      enabled ? '插件已启用' : '插件已停用',
      plugin.name,
    );
  };

  const reload = async (plugin: PluginStatus) => {
    await runOperation(
      plugin.id,
      'reload',
      () => api.reloadPlugin(plugin.id),
      '插件已热加载',
      plugin.name,
    );
  };

  const install = async (plugin: AvailablePlugin) => {
    await runOperation(
      plugin.id,
      'install',
      () => api.installPlugin(plugin.id),
      '插件已安装',
      `${plugin.name} ${plugin.version}`,
    );
  };

  const upgrade = async (plugin: PluginStatus) => {
    const release = availableById.get(plugin.id);
    await runOperation(
      plugin.id,
      'upgrade',
      () => api.upgradePlugin(plugin.id),
      '插件已升级',
      release ? `${plugin.name} ${release.version}` : plugin.name,
    );
  };

  const rollback = async (plugin: PluginStatus) => {
    await runOperation(
      plugin.id,
      'rollback',
      () => api.rollbackPlugin(plugin.id),
      '插件已回滚',
      plugin.name,
    );
  };

  const confirmUninstall = async () => {
    if (!uninstallTarget) return;
    const target = uninstallTarget;
    const succeeded = await runOperation(
      target.id,
      'uninstall',
      () => api.uninstallPlugin(target.id, keepData),
      '插件已卸载',
      target.name,
    );
    if (succeeded) setUninstallTarget(null);
  };

  const isBusy = activeOperation !== null;

  return (
    <TooltipProvider>
      <div className="flex h-full min-w-0 flex-col overflow-hidden">
        <div className="flex min-h-16 shrink-0 items-center gap-3 border-b px-4 sm:px-6">
          <div className="min-w-0">
            <h2 className="text-base font-semibold">插件管理</h2>
            <p className="truncate text-xs text-muted-foreground">
              {plugins.length} 个已安装，{available.length} 个可用
            </p>
          </div>
          <IconAction
            label="刷新插件目录和状态"
            onClick={() => void refresh()}
            disabled={loading || isBusy}
            className="ml-auto"
          >
            <RefreshCw className={loading ? 'animate-spin' : ''} />
          </IconAction>
        </div>

        <Tabs defaultValue="installed" className="flex min-h-0 flex-1 flex-col">
          <div className="shrink-0 border-b px-4 py-2 sm:px-6">
            <TabsList className="grid h-9 w-full max-w-72 grid-cols-2">
              <TabsTrigger value="installed" className="py-1 text-xs">
                已安装
              </TabsTrigger>
              <TabsTrigger value="available" className="py-1 text-xs">
                可安装
              </TabsTrigger>
            </TabsList>
          </div>

          <TabsContent value="installed" className="m-0 min-h-0 flex-1 overflow-y-auto px-4 sm:px-6">
            {loading && plugins.length === 0 ? (
              <LoadingState label="正在读取插件状态" />
            ) : plugins.length === 0 ? (
              <EmptyState label="暂无已安装插件" />
            ) : (
              plugins.map((plugin, index) => (
                <InstalledPluginRow
                  key={plugin.id}
                  plugin={plugin}
                  release={availableById.get(plugin.id)}
                  index={index}
                  activeOperation={activeOperation}
                  disabled={isBusy}
                  onToggle={toggleEnabled}
                  onReload={reload}
                  onUpgrade={upgrade}
                  onRollback={rollback}
                  onUninstall={(target) => {
                    setKeepData(true);
                    setUninstallTarget(target);
                  }}
                />
              ))
            )}
          </TabsContent>

          <TabsContent value="available" className="m-0 min-h-0 flex-1 overflow-y-auto px-4 sm:px-6">
            {loading && available.length === 0 ? (
              <LoadingState label="正在读取 OSS 插件目录" />
            ) : catalogError ? (
              <div className="flex min-h-32 items-start gap-3 py-8 text-sm text-destructive">
                <AlertCircle className="mt-0.5 h-4 w-4 shrink-0" />
                <span className="break-all">{catalogError}</span>
              </div>
            ) : available.length === 0 ? (
              <EmptyState label="暂无可安装插件" />
            ) : (
              available.map((plugin, index) => (
                <AvailablePluginRow
                  key={plugin.id}
                  plugin={plugin}
                  index={index}
                  activeOperation={activeOperation}
                  disabled={isBusy}
                  onInstall={install}
                />
              ))
            )}
          </TabsContent>
        </Tabs>
      </div>

      <Dialog open={uninstallTarget !== null} onOpenChange={(open) => !open && setUninstallTarget(null)}>
        <DialogContent className="mx-4 w-[calc(100%-2rem)] max-w-md">
          <DialogHeader>
            <DialogTitle>卸载 {uninstallTarget?.name}</DialogTitle>
            <DialogDescription>插件能力将立即从当前应用中移除。</DialogDescription>
          </DialogHeader>
          <div className="flex items-center justify-between gap-4 rounded-md border p-3">
            <div className="min-w-0">
              <p className="text-sm font-medium">保留插件数据</p>
              <p className="text-xs text-muted-foreground">重新安装后可继续使用现有数据</p>
            </div>
            <Switch
              checked={keepData}
              onCheckedChange={setKeepData}
              disabled={activeOperation?.operation === 'uninstall'}
              aria-label="保留插件数据"
            />
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={() => setUninstallTarget(null)} disabled={isBusy}>
              取消
            </Button>
            <Button variant="destructive" onClick={() => void confirmUninstall()} disabled={isBusy}>
              {activeOperation?.operation === 'uninstall' && <Loader2 className="animate-spin" />}
              卸载
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </TooltipProvider>
  );
}

function InstalledPluginRow({
  plugin,
  release,
  index,
  activeOperation,
  disabled,
  onToggle,
  onReload,
  onUpgrade,
  onRollback,
  onUninstall,
}: {
  plugin: PluginStatus;
  release?: AvailablePlugin;
  index: number;
  activeOperation: ActiveOperation | null;
  disabled: boolean;
  onToggle: (plugin: PluginStatus, enabled: boolean) => Promise<void>;
  onReload: (plugin: PluginStatus) => Promise<void>;
  onUpgrade: (plugin: PluginStatus) => Promise<void>;
  onRollback: (plugin: PluginStatus) => Promise<void>;
  onUninstall: (plugin: PluginStatus) => void;
}) {
  const working = activeOperation?.pluginId === plugin.id;
  const healthy = plugin.state === 'loaded';
  return (
    <div>
      {index > 0 && <Separator />}
      <div className="flex min-h-32 items-start gap-3 py-5">
        <div className="mt-0.5 shrink-0">
          {healthy ? (
            <CheckCircle2 className="h-5 w-5 text-emerald-500" />
          ) : (
            <AlertCircle
              className={`h-5 w-5 ${plugin.state === 'disabled' ? 'text-muted-foreground' : 'text-amber-500'}`}
            />
          )}
        </div>
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="break-words text-sm font-medium">{plugin.name}</span>
            <Badge variant={healthy ? 'secondary' : 'outline'}>{stateLabel[plugin.state]}</Badge>
            {release?.update_available && <Badge variant="outline">可升级 {release.version}</Badge>}
          </div>
          <div className="mt-1 flex flex-wrap gap-x-4 gap-y-1 text-xs text-muted-foreground">
            <span>清单 {plugin.manifest_version}</span>
            <span>运行 {plugin.loaded_version ?? '未加载'}</span>
            <span>第 {plugin.generation} 代</span>
            {plugin.has_sidecar && (
              <span>Sidecar {plugin.sidecar_running ? '运行中' : '未运行'}</span>
            )}
          </div>
          {plugin.last_error && (
            <p className="mt-2 break-words text-xs text-destructive">{plugin.last_error}</p>
          )}
          <div className="mt-3 flex flex-wrap items-center gap-1">
            <IconAction
              label="热加载插件"
              onClick={() => void onReload(plugin)}
              disabled={disabled || !plugin.enabled}
              working={working && activeOperation?.operation === 'reload'}
            >
              <RotateCw />
            </IconAction>
            <IconAction
              label="升级插件"
              onClick={() => void onUpgrade(plugin)}
              disabled={disabled || !release?.update_available || !release.supported}
              working={working && activeOperation?.operation === 'upgrade'}
            >
              <ArrowUpCircle />
            </IconAction>
            <IconAction
              label="回滚插件"
              onClick={() => void onRollback(plugin)}
              disabled={disabled || !plugin.can_rollback}
              working={working && activeOperation?.operation === 'rollback'}
            >
              <Undo2 />
            </IconAction>
            <IconAction
              label="卸载插件"
              onClick={() => onUninstall(plugin)}
              disabled={disabled}
              destructive
            >
              <Trash2 />
            </IconAction>
          </div>
        </div>
        <div className="flex h-8 shrink-0 items-center">
          {working && ['enable', 'disable'].includes(activeOperation?.operation ?? '') ? (
            <Loader2 className="h-4 w-4 animate-spin text-muted-foreground" />
          ) : (
            <Switch
              checked={plugin.enabled}
              onCheckedChange={(enabled) => void onToggle(plugin, enabled)}
              disabled={disabled}
              aria-label={`${plugin.enabled ? '停用' : '启用'} ${plugin.name}`}
            />
          )}
        </div>
      </div>
    </div>
  );
}

function AvailablePluginRow({
  plugin,
  index,
  activeOperation,
  disabled,
  onInstall,
}: {
  plugin: AvailablePlugin;
  index: number;
  activeOperation: ActiveOperation | null;
  disabled: boolean;
  onInstall: (plugin: AvailablePlugin) => Promise<void>;
}) {
  const installing = activeOperation?.pluginId === plugin.id && activeOperation.operation === 'install';
  return (
    <div>
      {index > 0 && <Separator />}
      <div className="flex min-h-28 items-start gap-4 py-5">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="break-words text-sm font-medium">{plugin.name}</span>
            <Badge variant="outline">{plugin.version}</Badge>
            {!plugin.supported && <Badge variant="outline">当前平台不可用</Badge>}
            {plugin.installed_version && <Badge variant="secondary">已安装</Badge>}
          </div>
          {plugin.description && (
            <p className="mt-2 break-words text-xs leading-5 text-muted-foreground">
              {plugin.description}
            </p>
          )}
        </div>
        <IconAction
          label="安装插件"
          onClick={() => void onInstall(plugin)}
          disabled={disabled || !plugin.supported || plugin.installed_version !== null}
          working={installing}
        >
          <Download />
        </IconAction>
      </div>
    </div>
  );
}

function IconAction({
  label,
  working = false,
  destructive = false,
  className,
  children,
  ...props
}: React.ButtonHTMLAttributes<HTMLButtonElement> & {
  label: string;
  working?: boolean;
  destructive?: boolean;
}) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          className={`h-8 w-8 shrink-0 ${destructive ? 'hover:bg-destructive/10 hover:text-destructive' : ''} ${className ?? ''}`}
          aria-label={label}
          {...props}
        >
          {working ? <Loader2 className="animate-spin" /> : children}
        </Button>
      </TooltipTrigger>
      <TooltipContent>{label}</TooltipContent>
    </Tooltip>
  );
}

function LoadingState({ label }: { label: string }) {
  return (
    <div className="flex h-32 items-center justify-center text-sm text-muted-foreground">
      <Loader2 className="mr-2 h-4 w-4 animate-spin" />
      {label}
    </div>
  );
}

function EmptyState({ label }: { label: string }) {
  return (
    <div className="flex h-32 items-center justify-center text-sm text-muted-foreground">{label}</div>
  );
}

function operationLabel(operation: Operation) {
  const labels: Record<Operation, string> = {
    enable: '启用',
    disable: '停用',
    install: '安装',
    reload: '热加载',
    upgrade: '升级',
    rollback: '回滚',
    uninstall: '卸载',
  };
  return labels[operation];
}
