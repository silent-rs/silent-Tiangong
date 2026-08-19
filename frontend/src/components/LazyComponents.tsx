import { lazy, Suspense } from 'react';
import { Loader2 } from 'lucide-react';

// 懒加载组件 - 使用命名导入
import { MessageList as MessageListComponent } from './MessageList';
import { MessageInput as MessageInputComponent } from './MessageInput';
import { PlanPanel as PlanPanelComponent } from './PlanPanel';
import { StatusPanel as StatusPanelComponent } from './StatusPanel';
import { AgentPanel as AgentPanelComponent } from './AgentPanel';

// 将命名导出包装为默认导出的懒加载组件
export const MessageList = lazy(() => Promise.resolve({ default: MessageListComponent }));
export const MessageInput = lazy(() => Promise.resolve({ default: MessageInputComponent }));
export const PlanPanel = lazy(() => Promise.resolve({ default: PlanPanelComponent }));
export const StatusPanel = lazy(() => Promise.resolve({ default: StatusPanelComponent }));
export const AgentPanel = lazy(() => Promise.resolve({ default: AgentPanelComponent }));

// 加载占位组件
function ComponentLoader() {
  return (
    <div className="flex items-center justify-center h-full">
      <Loader2 className="w-8 h-8 animate-spin text-primary" />
    </div>
  );
}

// 懒加载包装器
export function LazyMessageList() {
  return (
    <Suspense fallback={<ComponentLoader />}>
      <MessageList />
    </Suspense>
  );
}

type MessageInputProps = React.ComponentProps<typeof MessageInputComponent>;

export function LazyMessageInput(props: MessageInputProps) {
  return (
    <Suspense fallback={<ComponentLoader />}>
      <MessageInput {...props} />
    </Suspense>
  );
}

export function LazyPlanPanel() {
  return (
    <Suspense fallback={null}>
      <PlanPanel />
    </Suspense>
  );
}

type StatusPanelProps = React.ComponentProps<typeof StatusPanelComponent>;

export function LazyStatusPanel(props: StatusPanelProps) {
  return (
    <Suspense fallback={null}>
      <StatusPanel {...props} />
    </Suspense>
  );
}

export function LazyAgentPanel() {
  return (
    <Suspense fallback={null}>
      <AgentPanel />
    </Suspense>
  );
}
