import { useCallback, useEffect, useState } from 'react';
import { api, type ServerConfig } from '../../api/tauri';
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

      {!serverRunning && (
        <div className="mx-4 mt-2 px-3 py-2 rounded-md bg-amber-500/10 border border-amber-500/20 text-amber-600 dark:text-amber-400 text-sm">
          Server 未运行，定时任务和 Webhook 处于未激活状态。请在「Server」选项卡中启动 Server。
        </div>
      )}

      <div className="flex-1 min-h-0 overflow-y-auto">
        {subTab === 'jobs' && <JobPanel serverRunning={serverRunning} />}
        {subTab === 'webhooks' && <WebhookPanel serverRunning={serverRunning} />}
      </div>
    </div>
  );
}
