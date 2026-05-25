import { useCallback, useEffect, useState } from 'react';
import { api, type ServerConfig } from '../../api/tauri';
import { Switch } from '../ui/switch';
import { JobPanel } from './JobPanel';
import { WebhookPanel } from './WebhookPanel';

type SubTab = 'jobs' | 'webhooks';

const TAB_LABELS: Record<SubTab, string> = {
  jobs: '定时任务',
  webhooks: 'Webhook',
};

export function AutomationSettings() {
  const [subTab, setSubTab] = useState<SubTab>('jobs');
  const [serverRunning, setServerRunning] = useState(false);
  const [isToggling, setIsToggling] = useState(false);

  const checkServer = useCallback(async () => {
    try {
      const cfg: ServerConfig = await api.getServerConfig();
      setServerRunning(cfg.running);
    } catch {
      setServerRunning(false);
    }
  }, []);

  useEffect(() => {
    checkServer();
    const timer = window.setInterval(checkServer, 5000);
    return () => window.clearInterval(timer);
  }, [checkServer]);

  const handleToggleServer = async (enabled: boolean) => {
    setIsToggling(true);
    try {
      if (enabled) {
        await api.startServer();
      } else {
        await api.stopServer();
      }
      await checkServer();
    } catch (e) {
      console.error('切换 Server 失败:', e);
    } finally {
      setIsToggling(false);
    }
  };

  return (
    <div className="flex flex-col h-full">
      <div className="flex gap-1 shrink-0 p-4 pb-0">
        {(['jobs', 'webhooks'] as const).map((tab) => (
          <button
            key={tab}
            className={`px-3 py-1.5 text-sm rounded-md transition-colors ${
              subTab === tab
                ? 'bg-primary text-primary-foreground'
                : 'bg-muted text-muted-foreground hover:text-foreground'
            }`}
            onClick={() => setSubTab(tab)}
          >
            {TAB_LABELS[tab]}
          </button>
        ))}
      </div>

      <div className="flex items-center gap-2 mx-4 mt-3 px-3 py-2 rounded-md bg-muted/50">
        <span
          className={`inline-block w-2 h-2 rounded-full shrink-0 ${
            serverRunning ? 'bg-green-500' : 'bg-muted-foreground/40'
          }`}
        />
        <span className="text-sm text-muted-foreground flex-1">
          {serverRunning ? 'Server 运行中' : 'Server 未启动'}
        </span>
        <Switch
          checked={serverRunning}
          onCheckedChange={handleToggleServer}
          disabled={isToggling}
        />
      </div>

      <div className="flex-1 min-h-0 overflow-y-auto">
        {subTab === 'jobs' && <JobPanel serverRunning={serverRunning} />}
        {subTab === 'webhooks' && <WebhookPanel serverRunning={serverRunning} />}
      </div>
    </div>
  );
}
