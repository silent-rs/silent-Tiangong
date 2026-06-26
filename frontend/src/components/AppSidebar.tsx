import { useStore } from '@/store/useStore';
import { Button } from './ui/button';
import { ScrollArea } from './ui/scroll-area';
import { useSidebar } from './ui/sidebar';
import { Plus, Trash2, ChevronRight, ChevronDown, Folder } from 'lucide-react';
import { useMemo, useState } from 'react';
import { SettingsDialog } from './SettingsDialog';
import type { Session } from '@/api/tauri';

/** 默认分组 key：与全局 workspace 一致（或 cwd 为空）的会话归入此分组，平铺不显示分组头 */
const DEFAULT_GROUP_KEY = '__default__';
/** 收缩状态下每个分组最多显示的会话数 */
const COLLAPSED_LIMIT = 5;

interface SessionGroup {
  /** 分组 key：默认分组为 __default__，其余为会话 cwd */
  key: string;
  /** 分组显示名：默认分组为空字符串（不显示头），其余为 cwd 的 basename */
  label: string;
  /** 完整 cwd 路径，用作 title 提示 */
  fullPath: string;
  isDefault: boolean;
  /** 组内会话，已按 updated_at 倒序 */
  sessions: Session[];
}

/** 取路径末段作为分组显示名，兼容 / 与 \ */
function basename(path: string): string {
  if (!path) return '';
  const trimmed = path.replace(/[\\/]+$/, '');
  const parts = trimmed.split(/[\\/]/);
  return parts[parts.length - 1] || trimmed;
}

/** 是否应归入默认分组：cwd 为空或与全局 workspace 一致 */
function isDefaultCwd(cwd: string, workspaceDir: string): boolean {
  return !cwd || cwd === workspaceDir;
}

/**
 * 按 workspace(cwd) 对会话分组。
 * - 默认分组（cwd 与全局 workspace 一致或为空）排最前且不显示分组头。
 * - 其余分组按"组内最新 updated_at"倒序排列。
 * - 每个分组内部按 updated_at 倒序。
 */
function groupSessions(sessions: Session[], workspaceDir: string): SessionGroup[] {
  const map = new Map<string, Session[]>();
  for (const session of sessions) {
    const key = isDefaultCwd(session.cwd, workspaceDir) ? DEFAULT_GROUP_KEY : session.cwd;
    const list = map.get(key);
    if (list) {
      list.push(session);
    } else {
      map.set(key, [session]);
    }
  }

  const groups: SessionGroup[] = [];
  for (const [key, list] of map) {
    list.sort((a, b) => b.updated_at.localeCompare(a.updated_at));
    if (key === DEFAULT_GROUP_KEY) {
      groups.push({ key, label: '', fullPath: workspaceDir, isDefault: true, sessions: list });
    } else {
      groups.push({ key, label: basename(key), fullPath: key, isDefault: false, sessions: list });
    }
  }

  // 默认分组置顶，其余按组内最新 updated_at 倒序
  groups.sort((a, b) => {
    if (a.isDefault !== b.isDefault) return a.isDefault ? -1 : 1;
    if (a.isDefault) return 0; // 两个默认分组不会同时存在，仅占位
    const aLatest = a.sessions[0]?.updated_at ?? '';
    const bLatest = b.sessions[0]?.updated_at ?? '';
    return bLatest.localeCompare(aLatest);
  });

  return groups;
}

export function AppSidebar() {
  const {
    sessions,
    activeSessionId,
    isDraft,
    isSending,
    sessionRunStatuses,
    createSession,
    switchSession,
    deleteSession,
    isLoadingSessions,
    workspaceDir,
  } = useStore();

  const { open } = useSidebar();
  const [showDeleteConfirm, setShowDeleteConfirm] = useState(false);
  // 每个分组的展开状态：默认收缩（只显示最近 COLLAPSED_LIMIT 个）；true=展开显示全部
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});

  // 仅展示有消息的会话或当前活跃会话
  const visibleSessions = useMemo(
    () => sessions.filter((s) => s.message_count > 0 || s.id === activeSessionId),
    [sessions, activeSessionId],
  );

  const groups = useMemo(
    () => groupSessions(visibleSessions, workspaceDir),
    [visibleSessions, workspaceDir],
  );
  // 是否存在非默认（workspace 分类）分组；没有时默认分组不收缩、全部平铺
  const hasWorkspaceGroups = groups.some((g) => !g.isDefault);

  const toggleGroup = (key: string) => {
    setExpanded((prev) => ({ ...prev, [key]: !prev[key] }));
  };

  const handleDeleteSession = async () => {
    await deleteSession();
    setShowDeleteConfirm(false);
  };

  const renderSessionItem = (session: Session) => {
    const isRunning = !!sessionRunStatuses[session.id];
    const isActive = !isDraft && activeSessionId === session.id;
    return (
      <button
        key={session.id}
        className={`w-full text-left px-3 py-1.5 rounded-md text-sm flex items-center gap-2 group transition-colors ${
          isActive
            ? 'bg-sidebar-accent text-sidebar-accent-foreground'
            : 'text-sidebar-foreground/70 hover:bg-sidebar-accent hover:text-sidebar-accent-foreground'
        }`}
        disabled={isSending}
        onClick={() => {
          if (!isSending && (session.id !== activeSessionId || isDraft)) {
            switchSession(session.id);
          }
        }}
      >
        <span className="flex-1 truncate">{session.title || '新对话'}</span>
        {isRunning && (
          <span className="w-2 h-2 rounded-full bg-yellow-500 animate-pulse shrink-0" />
        )}
        {isActive && (
          <button
            className="opacity-0 group-hover:opacity-100 hover:text-destructive transition-opacity"
            onClick={(e) => {
              e.stopPropagation();
              setShowDeleteConfirm(true);
            }}
          >
            <Trash2 className="w-3 h-3" />
          </button>
        )}
      </button>
    );
  };

  const renderGroup = (group: SessionGroup) => {
    const isExpanded = expanded[group.key];
    // 默认分组在无 workspace 分类分组时不收缩，全部平铺
    const allowCollapse = group.isDefault ? hasWorkspaceGroups : true;
    // 收缩态：只显示最近 COLLAPSED_LIMIT 个 + "显示全部"；展开态：全部 + "收起"
    const visibleItems = isExpanded || !allowCollapse
      ? group.sessions
      : group.sessions.slice(0, COLLAPSED_LIMIT);
    const canCollapse = allowCollapse && group.sessions.length > COLLAPSED_LIMIT;

    if (group.isDefault) {
      // 默认分组：无分组头，直接平铺
      return (
        <div key={group.key} className="space-y-1">
          {visibleItems.map(renderSessionItem)}
          {canCollapse && (
            <button
              className="w-full text-left px-3 py-1.5 rounded-md text-xs text-muted-foreground hover:text-foreground hover:bg-sidebar-accent/50 transition-colors"
              onClick={() => toggleGroup(group.key)}
            >
              {isExpanded ? '收起' : `显示全部 (${group.sessions.length})`}
            </button>
          )}
        </div>
      );
    }

    // 非默认分组：显示分组头 + 会话列表
    return (
      <div key={group.key} className="space-y-1">
        <div className="flex items-center gap-1 group">
          <button
            className="flex-1 flex items-center gap-1.5 px-2 py-1.5 rounded-md text-xs font-medium text-muted-foreground hover:text-foreground hover:bg-sidebar-accent/50 transition-colors min-w-0"
            onClick={() => canCollapse && toggleGroup(group.key)}
            title={group.fullPath}
            disabled={!canCollapse}
          >
            {isExpanded ? (
              <ChevronDown className="w-3 h-3 shrink-0" />
            ) : (
              <ChevronRight className="w-3 h-3 shrink-0" />
            )}
            <Folder className="w-3.5 h-3.5 shrink-0" />
            <span className="truncate">{group.label}</span>
            <span className="text-muted-foreground/70 shrink-0">{group.sessions.length}</span>
          </button>
          <button
            className="p-1 rounded text-muted-foreground opacity-0 group-hover:opacity-100 hover:text-foreground hover:bg-sidebar-accent/50 transition-opacity shrink-0"
            onClick={() => !isSending && createSession(group.fullPath)}
            disabled={isSending}
            title="在此 workspace 下新建对话"
          >
            <Plus className="w-3.5 h-3.5" />
          </button>
        </div>
        <div className="space-y-1 pl-1">
          {visibleItems.map(renderSessionItem)}
        </div>
        {canCollapse && (
          <button
            className="w-full text-left px-3 py-1.5 rounded-md text-xs text-muted-foreground hover:text-foreground hover:bg-sidebar-accent/50 transition-colors"
            onClick={() => toggleGroup(group.key)}
          >
            {isExpanded ? '收起' : `显示全部 (${group.sessions.length})`}
          </button>
        )}
      </div>
    );
  };

  const content = (
    <div className="flex flex-col h-full min-h-0 min-w-[var(--sidebar-width,16rem)]">
      {/* 新建会话 */}
      <div className="p-2 shrink-0">
        <Button
          variant="ghost"
          className="w-full justify-start"
          disabled={isSending}
          onClick={() => !isSending && createSession()}
        >
          <Plus className="w-4 h-4 mr-2" />
          新对话
        </Button>
      </div>

      {/* 会话列表（按 workspace 分组） */}
      <ScrollArea className="flex-1 min-h-0 pl-2 pr-1">
        <div className="space-y-2 py-1 pr-1">
          {isLoadingSessions ? (
            <div className="px-3 py-2 text-sm text-muted-foreground">加载中...</div>
          ) : groups.length === 0 ? (
            <div className="px-3 py-2 text-sm text-muted-foreground">暂无对话</div>
          ) : (
            groups.map(renderGroup)
          )}
        </div>
      </ScrollArea>

      {/* 底部设置 */}
      <div className="p-2 border-t border-sidebar-border shrink-0">
        <SettingsDialog />
      </div>
    </div>
  );

  const deleteConfirm = showDeleteConfirm && (
    <div className="fixed inset-0 z-[60] flex items-center justify-center bg-black/50">
      <div className="bg-popover border rounded-lg p-4 max-w-sm w-full mx-4">
        <h3 className="font-medium mb-2">删除对话</h3>
        <p className="text-muted-foreground text-sm mb-4">确定要删除当前对话吗？此操作无法撤销。</p>
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={() => setShowDeleteConfirm(false)}>
            取消
          </Button>
          <Button variant="destructive" onClick={handleDeleteSession}>
            删除
          </Button>
        </div>
      </div>
    </div>
  );

  return (
    <aside
      className="shrink-0 min-h-0 border-r bg-sidebar text-sidebar-foreground flex flex-col overflow-hidden transition-[width] duration-200 ease-linear"
      style={{ width: open ? 'var(--sidebar-width, 16rem)' : '0px' }}
    >
      {content}
      {deleteConfirm}
    </aside>
  );
}
