import { useCallback, useEffect, useMemo, useState } from 'react';
import {
  AlertCircle,
  ArrowUpCircle,
  BookOpen,
  CheckCircle2,
  Circle,
  Download,
  FolderInput,
  Loader2,
  RefreshCw,
  RotateCw,
  Trash2,
  Undo2,
} from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
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
  | 'import'
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

const PLUGIN_DEVELOPMENT_DOC_URL =
  'https://github.com/silent-rs/silent-Tiangong/blob/main/docs/plugin-development.md';

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

  const importLocal = async () => {
    let selected: string | string[] | null;
    try {
      selected = await open({
        directory: true,
        multiple: false,
        title: '选择本地插件目录',
      });
    } catch (error) {
      showError('选择失败', String(error));
      return;
    }
    if (typeof selected !== 'string') return;

    setActiveOperation({ pluginId: 'local', operation: 'import' });
    try {
      const plugin = await api.importLocalPlugin(selected);
      await finishOperation();
      showSuccess('插件已导入', `${plugin.name} ${plugin.manifest_version}`);
    } catch (error) {
      showError('导入失败', String(error));
      await refresh();
    } finally {
      setActiveOperation(null);
    }
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
      <div className="relative flex h-full min-w-0 flex-col overflow-hidden">
        <div className="flex min-h-16 shrink-0 items-center gap-3 border-b px-4 sm:px-6">
          <div className="min-w-0">
            <h2 className="text-base font-semibold">插件管理</h2>
            <p className="truncate text-xs text-muted-foreground">
              {plugins.length} 个已安装，{available.length} 个可用
            </p>
          </div>
          <div className="ml-auto flex shrink-0 items-center gap-1">
            <Tooltip>
              <TooltipTrigger asChild>
                <Button variant="ghost" size="icon" className="h-8 w-8" asChild>
                  <a
                    href={PLUGIN_DEVELOPMENT_DOC_URL}
                    target="_blank"
                    rel="noopener noreferrer"
                    aria-label="打开插件开发文档"
                  >
                    <BookOpen />
                  </a>
                </Button>
              </TooltipTrigger>
              <TooltipContent>插件开发文档</TooltipContent>
            </Tooltip>
            <IconAction
              label="导入本地插件"
              onClick={() => void importLocal()}
              disabled={loading || isBusy}
              working={activeOperation?.operation === 'import'}
            >
              <FolderInput />
            </IconAction>
            <IconAction
              label="刷新插件目录和状态"
              onClick={() => void refresh()}
              disabled={loading || isBusy}
            >
              <RefreshCw className={loading ? 'animate-spin' : ''} />
            </IconAction>
          </div>
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
              plugins.map((plugin) => (
                <InstalledPluginRow
                  key={plugin.id}
                  plugin={plugin}
                  release={availableById.get(plugin.id)}
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
        {loading && (
          <div
            className="absolute inset-0 z-50 flex items-center justify-center bg-background/75 backdrop-blur-[1px]"
            role="status"
            aria-live="polite"
            aria-label="正在刷新插件状态"
          >
            <div className="flex items-center gap-2 rounded-md border bg-background px-4 py-3 text-sm shadow-lg">
              <Loader2 className="h-4 w-4 animate-spin text-primary" />
              正在刷新插件目录和运行状态…
            </div>
          </div>
        )}
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
  const canUpgrade = Boolean(release?.update_available && release.supported);
  const currentVersion = plugin.loaded_version ?? plugin.manifest_version;
  const sidecar = getSidecarPresentation(plugin);
  const StatusIcon = healthy ? CheckCircle2 : plugin.state === 'disabled' ? Circle : AlertCircle;

  return (
    <div className="border-b py-4 last:border-b-0">
      <div className="flex items-start gap-3 rounded-lg border border-transparent px-3 py-3 transition-colors hover:border-border/70 hover:bg-muted/20">
        <StatusIcon
          className={`mt-0.5 h-5 w-5 shrink-0 ${
            healthy
              ? 'text-emerald-500'
              : plugin.state === 'disabled'
                ? 'text-muted-foreground'
                : 'text-amber-500'
          }`}
        />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="break-words text-sm font-medium">{plugin.name}</span>
            <Badge variant={healthy ? 'secondary' : 'outline'}>{stateLabel[plugin.state]}</Badge>
            {canUpgrade && <Badge className="border-primary/30 bg-primary/10 text-primary">发现新版本</Badge>}
          </div>

          <div className="mt-2 flex flex-wrap items-center gap-x-5 gap-y-2 text-xs">
            <VersionInfo label="当前版本" value={currentVersion} />
            {canUpgrade && release && (
              <VersionInfo label="可升级至" value={release.version} emphasized />
            )}
            <span className={`inline-flex items-center gap-1.5 ${sidecar.className}`}>
              <span className={`h-1.5 w-1.5 rounded-full ${sidecar.dotClassName}`} />
              {sidecar.label}
            </span>
          </div>

          {plugin.last_error && (
            <p className="mt-2 break-words text-xs leading-5 text-destructive">{plugin.last_error}</p>
          )}

          <div className="mt-3 flex flex-wrap items-center gap-2">
            {canUpgrade && (
              <Button
                size="sm"
                className="h-8"
                onClick={() => void onUpgrade(plugin)}
                disabled={disabled}
              >
                {working && activeOperation?.operation === 'upgrade' ? (
                  <Loader2 className="animate-spin" />
                ) : (
                  <ArrowUpCircle />
                )}
                升级到 {release?.version}
              </Button>
            )}
            <IconAction
              label="重新加载插件"
              onClick={() => void onReload(plugin)}
              disabled={disabled || !plugin.enabled}
              working={working && activeOperation?.operation === 'reload'}
            >
              <RotateCw />
            </IconAction>
            <IconAction
              label="回滚到上一版本"
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

        <div className="flex shrink-0 items-center gap-2 pl-2">
          <span className="hidden text-xs text-muted-foreground sm:inline">
            {plugin.enabled ? '已启用' : '已停用'}
          </span>
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

function VersionInfo({
  label,
  value,
  emphasized = false,
}: {
  label: string;
  value: string;
  emphasized?: boolean;
}) {
  return (
    <span className="inline-flex items-center gap-1.5 text-muted-foreground">
      <span>{label}</span>
      <span className={emphasized ? 'font-medium text-primary' : 'font-medium text-foreground'}>
        {value}
      </span>
    </span>
  );
}

function getSidecarPresentation(plugin: PluginStatus) {
  if (!plugin.has_sidecar) {
    return {
      label: '无需后台服务',
      className: 'text-muted-foreground',
      dotClassName: 'bg-muted-foreground/60',
    };
  }
  if (plugin.sidecar_running) {
    return {
      label: '后台服务运行中',
      className: 'text-emerald-500',
      dotClassName: 'bg-emerald-500',
    };
  }
  if (!plugin.enabled || plugin.state === 'disabled') {
    return {
      label: '后台服务已停用',
      className: 'text-muted-foreground',
      dotClassName: 'bg-muted-foreground/60',
    };
  }
  if (plugin.last_error || plugin.state === 'error' || plugin.state === 'degraded') {
    return {
      label: '后台服务异常',
      className: 'text-destructive',
      dotClassName: 'bg-destructive',
    };
  }
  return {
    label: '后台服务按需启动',
    className: 'text-muted-foreground',
    dotClassName: 'bg-amber-500',
  };
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
    import: '导入',
    install: '安装',
    reload: '热加载',
    upgrade: '升级',
    rollback: '回滚',
    uninstall: '卸载',
  };
  return labels[operation];
}
