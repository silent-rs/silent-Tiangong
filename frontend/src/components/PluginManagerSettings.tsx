import { useCallback, useEffect, useState } from 'react';
import { AlertCircle, CheckCircle2, Loader2, RefreshCw, RotateCw } from 'lucide-react';
import { api, type PluginContributionEntry, type PluginStatus } from '@/api/tauri';
import { Button } from './ui/button';
import { Badge } from './ui/badge';
import { Separator } from './ui/separator';
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from './ui/tooltip';
import { useToast } from './Toast';

type Props = {
  onContributionsChanged: (entries: PluginContributionEntry[]) => void;
};

const stateLabel: Record<PluginStatus['state'], string> = {
  loaded: '已加载',
  degraded: '运行异常',
  error: '加载失败',
};

export function PluginManagerSettings({ onContributionsChanged }: Props) {
  const [plugins, setPlugins] = useState<PluginStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [reloadingId, setReloadingId] = useState<string | null>(null);
  const { showError, showSuccess } = useToast();

  const refresh = useCallback(async () => {
    setLoading(true);
    try {
      setPlugins(await api.listPlugins());
    } catch (error) {
      showError('读取失败', String(error));
    } finally {
      setLoading(false);
    }
  }, [showError]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const reload = async (plugin: PluginStatus) => {
    setReloadingId(plugin.id);
    try {
      const status = await api.reloadPlugin(plugin.id);
      setPlugins((current) => current.map((item) => (item.id === status.id ? status : item)));
      const contributions = await api.listPluginContributions();
      onContributionsChanged(contributions.filter((entry) => entry.has_view));
      showSuccess('插件已热加载', `${status.name} 已切换到第 ${status.generation} 代`);
    } catch (error) {
      showError('热加载失败', `${plugin.name} 仍继续使用原有版本：${String(error)}`);
      await refresh();
    } finally {
      setReloadingId(null);
    }
  };

  return (
    <TooltipProvider>
      <div className="flex h-full flex-col overflow-hidden">
        <div className="flex h-16 shrink-0 items-center border-b px-6">
          <div>
            <h2 className="text-base font-semibold">插件管理</h2>
            <p className="text-xs text-muted-foreground">{plugins.length} 个已安装插件</p>
          </div>
          <Tooltip>
            <TooltipTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                className="ml-auto h-8 w-8"
                onClick={() => void refresh()}
                disabled={loading || reloadingId !== null}
                aria-label="刷新插件状态"
              >
                <RefreshCw className={`h-4 w-4 ${loading ? 'animate-spin' : ''}`} />
              </Button>
            </TooltipTrigger>
            <TooltipContent>刷新插件状态</TooltipContent>
          </Tooltip>
        </div>

        <div className="flex-1 overflow-y-auto px-6">
          {loading && plugins.length === 0 ? (
            <div className="flex h-32 items-center justify-center text-sm text-muted-foreground">
              <Loader2 className="mr-2 h-4 w-4 animate-spin" />
              正在读取插件状态
            </div>
          ) : plugins.length === 0 ? (
            <div className="flex h-32 items-center justify-center text-sm text-muted-foreground">
              暂无已安装插件
            </div>
          ) : (
            plugins.map((plugin, index) => {
              const isReloading = reloadingId === plugin.id;
              const isHealthy = plugin.state === 'loaded';
              return (
                <div key={plugin.id}>
                  {index > 0 && <Separator />}
                  <div className="flex min-h-28 items-start gap-4 py-5">
                    <div className="mt-0.5 shrink-0">
                      {isHealthy ? (
                        <CheckCircle2 className="h-5 w-5 text-emerald-500" />
                      ) : (
                        <AlertCircle className="h-5 w-5 text-amber-500" />
                      )}
                    </div>
                    <div className="min-w-0 flex-1">
                      <div className="flex flex-wrap items-center gap-2">
                        <span className="text-sm font-medium">{plugin.name}</span>
                        <Badge variant={isHealthy ? 'secondary' : 'outline'}>
                          {stateLabel[plugin.state]}
                        </Badge>
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
                    </div>
                    <Tooltip>
                      <TooltipTrigger asChild>
                        <Button
                          variant="outline"
                          size="icon"
                          className="h-8 w-8 shrink-0"
                          onClick={() => void reload(plugin)}
                          disabled={reloadingId !== null}
                          aria-label={`热加载 ${plugin.name}`}
                        >
                          {isReloading ? (
                            <Loader2 className="h-4 w-4 animate-spin" />
                          ) : (
                            <RotateCw className="h-4 w-4" />
                          )}
                        </Button>
                      </TooltipTrigger>
                      <TooltipContent>热加载插件</TooltipContent>
                    </Tooltip>
                  </div>
                </div>
              );
            })
          )}
        </div>
      </div>
    </TooltipProvider>
  );
}
