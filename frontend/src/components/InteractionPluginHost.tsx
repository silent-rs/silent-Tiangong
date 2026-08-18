import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { api, type SlotContributionEntry } from '@/api/tauri';
import { useStore } from '@/store/useStore';
import { PluginSandbox } from './PluginSandbox';

interface ToolRequestedEvent {
  invocation_id: string;
  session_id: string;
}

interface ToolClosedEvent {
  invocation_id: string;
  status: 'answered' | 'expired' | 'cancelled';
}

interface InteractionPluginHostProps {
  onVisibilityChange?: (visible: boolean) => void;
}

/**
 * Desktop 交互插件宿主。容器显示时覆盖输入区，iframe 始终挂载以持续
 * 接收工具调用；宿主只根据调用所属会话控制显隐，不解析业务参数。
 */
export function InteractionPluginHost({ onVisibilityChange }: InteractionPluginHostProps) {
  const activeSessionId = useStore((state) => state.activeSessionId);
  const newConversationId = useStore((state) => state.newConversationId);
  const currentSessionId = activeSessionId ?? newConversationId;
  const [handler, setHandler] = useState<SlotContributionEntry | null>(null);
  const [html, setHtml] = useState('');
  const [loadError, setLoadError] = useState('');
  const [, setPendingVersion] = useState(0);
  const [recentSessions, setRecentSessions] = useState<Set<string>>(() => new Set());
  const invocationSessions = useRef(new Map<string, string>());
  const closeTimers = useRef(new Map<string, number>());

  useEffect(() => {
    let disposed = false;
    let loadVersion = 0;

    const refresh = async () => {
      const version = ++loadVersion;
      try {
        const items = await api.listSlotContributions('session.interaction');
        const selected = items[0] ?? null;
        if (disposed || version !== loadVersion) return;
        if (!selected) {
          setHandler(null);
          setHtml('');
          setLoadError('');
          return;
        }
        setHandler(selected);
        const view = selected.source === 'manifest'
          ? await api.pluginOpenEntry(selected.plugin_id, selected.contribution_id)
          : await api.pluginOpenView(selected.plugin_id, selected.contribution_id);
        if (disposed || version !== loadVersion) return;
        setHtml(view);
        setLoadError('');
      } catch (error) {
        if (!disposed && version === loadVersion) {
          setHtml('');
          setLoadError(String(error));
          console.warn('[interaction-plugin] 加载处理器失败', error);
        }
      }
    };

    const onPluginChanged = () => { void refresh(); };
    void refresh();
    window.addEventListener('tiangong:plugin-changed', onPluginChanged);
    return () => {
      disposed = true;
      window.removeEventListener('tiangong:plugin-changed', onPluginChanged);
    };
  }, []);

  useEffect(() => {
    if (!handler) return;
    let disposed = false;
    let stopEvents: (() => void) | null = null;
    let requestedSubscribed = false;
    let closedSubscribed = false;

    invocationSessions.current.clear();
    setPendingVersion((value) => value + 1);
    setRecentSessions(new Set());

    const markRecentlyClosed = (sessionId: string) => {
      setRecentSessions((current) => new Set(current).add(sessionId));
      const previous = closeTimers.current.get(sessionId);
      if (previous) window.clearTimeout(previous);
      const timer = window.setTimeout(() => {
        closeTimers.current.delete(sessionId);
        setRecentSessions((current) => {
          const next = new Set(current);
          next.delete(sessionId);
          return next;
        });
      }, 1600);
      closeTimers.current.set(sessionId, timer);
    };

    void (async () => {
      stopEvents = await api.onBridgeEvent((event) => {
        if (event.plugin_id !== handler.plugin_id) return;
        try {
          if (event.channel === 'tool.requested') {
            const requested = JSON.parse(event.payload) as ToolRequestedEvent;
            if (!requested.invocation_id || !requested.session_id) return;
            invocationSessions.current.set(requested.invocation_id, requested.session_id);
            setPendingVersion((value) => value + 1);
          } else if (event.channel === 'tool.closed') {
            const closed = JSON.parse(event.payload) as ToolClosedEvent;
            const sessionId = invocationSessions.current.get(closed.invocation_id);
            if (!sessionId) return;
            invocationSessions.current.delete(closed.invocation_id);
            setPendingVersion((value) => value + 1);
            markRecentlyClosed(sessionId);
          }
        } catch (error) {
          console.warn('[interaction-plugin] 忽略无效工具事件', error);
        }
      });
      if (disposed) {
        stopEvents();
        stopEvents = null;
        return;
      }

      await api.bridgeSubscribe(handler.plugin_id, 'tool.requested');
      if (disposed) {
        await api.bridgeUnsubscribe(handler.plugin_id, 'tool.requested');
        return;
      }
      requestedSubscribed = true;
      await api.bridgeSubscribe(handler.plugin_id, 'tool.closed');
      if (disposed) {
        await api.bridgeUnsubscribe(handler.plugin_id, 'tool.closed');
        return;
      }
      closedSubscribed = true;
    })().catch((error) => {
      if (!disposed) console.warn('[interaction-plugin] 订阅工具事件失败', error);
    });

    return () => {
      disposed = true;
      stopEvents?.();
      if (requestedSubscribed) {
        void api.bridgeUnsubscribe(handler.plugin_id, 'tool.requested');
      }
      if (closedSubscribed) {
        void api.bridgeUnsubscribe(handler.plugin_id, 'tool.closed');
      }
      for (const timer of closeTimers.current.values()) window.clearTimeout(timer);
      closeTimers.current.clear();
      invocationSessions.current.clear();
    };
  }, [handler]);

  const hasPending = currentSessionId
    ? [...invocationSessions.current.values()].some((sessionId) => sessionId === currentSessionId)
    : false;
  const visible = Boolean(currentSessionId && (hasPending || recentSessions.has(currentSessionId)));

  useLayoutEffect(() => {
    onVisibilityChange?.(visible);
  }, [onVisibilityChange, visible]);

  useEffect(() => () => {
    onVisibilityChange?.(false);
  }, [onVisibilityChange]);

  if (!handler) return null;

  return (
    <div
      aria-hidden={!visible}
      aria-label="用户交互"
      aria-modal={visible || undefined}
      role="dialog"
      className={visible
        ? 'absolute inset-0 z-[60] bg-background opacity-100 transition-opacity duration-150'
        : 'pointer-events-none invisible absolute inset-0 z-[60] opacity-0 transition-opacity duration-150'}
    >
      <div className="mx-auto h-full w-full max-w-3xl p-4">
        <div className="h-full w-full overflow-hidden rounded-md border bg-card shadow-sm">
          {html ? (
            <PluginSandbox
              pluginId={handler.plugin_id}
              contributionId={handler.contribution_id}
              sandbox={handler.sandbox}
              html={html}
              sessionId={currentSessionId}
            />
          ) : visible ? (
            <div className="p-3 text-sm text-destructive">
              交互处理器页面加载失败{loadError ? `：${loadError}` : ''}
            </div>
          ) : null}
        </div>
      </div>
    </div>
  );
}
