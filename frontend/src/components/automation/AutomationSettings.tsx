import { useState } from 'react';
import { JobPanel } from './JobPanel';
import { WebhookPanel } from './WebhookPanel';

type SubTab = 'jobs' | 'webhooks';

const TAB_LABELS: Record<SubTab, string> = {
  jobs: '定时任务',
  webhooks: 'Webhook',
};

export function AutomationSettings() {
  const [subTab, setSubTab] = useState<SubTab>('jobs');

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

      <div className="flex-1 min-h-0 overflow-y-auto">
        {subTab === 'jobs' && <JobPanel />}
        {subTab === 'webhooks' && <WebhookPanel />}
      </div>
    </div>
  );
}
