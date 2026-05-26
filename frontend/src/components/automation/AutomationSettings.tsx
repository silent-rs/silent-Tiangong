import { JobPanel } from './JobPanel';

export function AutomationSettings() {
  return (
    <div className="flex flex-col h-full">
      <div className="flex-1 min-h-0 overflow-y-auto">
        <JobPanel />
      </div>
    </div>
  );
}
