import { useEffect, useState } from 'react';
import { Bot, Globe, Puzzle, TerminalSquare } from 'lucide-react';
import { BUILTIN_TAB_KIND_MULTI, TAB_KIND_NAME, api } from '@/api/tauri';
import type { AppEntry, TabKind } from '@/api/tauri';
import {
  ContextMenu,
  ContextMenuContent,
  ContextMenuItem,
  ContextMenuSeparator,
  ContextMenuTrigger,
} from './ui/context-menu';

/**
 * 拓展区 App 矩阵（矩阵态视图）。
 *
 * 统一 App 目录：官方内置 App（浏览器/终端/Agent Team）与三方 extension.tab
 * 贡献同构展示（启动台网格，图标块 + 名称），数据源 listExtensionApps。
 * 图标右上角绿点 = 该 App 存在已打开实例；左键打开；右键菜单按实例模式区分。
 */
export interface ExtensionMatrixProps {
  /** 打开官方浏览器/终端 App（进入既有 App 态）。 */
  onOpenApp: (kind: TabKind) => void;
  /** 当前会话存在已打开实例的官方 App（图标右上角显示「在用」绿点）。 */
  runningKinds?: TabKind[];
  /** 当前会话存在已打开实例的 plugin App 键（`plugin_id:contribution_id`）。 */
  runningPluginApps?: string[];
  /** 多实例 App 新建实例（宿主切换 App 态并下发新建命令）。 */
  onNewAppTab?: (kind: TabKind) => void;
  /** 关闭官方 App 的全部已打开实例（无实例时不显示菜单项）。 */
  onCloseApp?: (kind: TabKind) => void;
  /** 打开三方或官方 plugin 形态的 App（agent-team 等，按 open_mode 分派）。 */
  onOpenPluginApp?: (app: AppEntry) => void;
}

/** App 图标：官方按贡献映射，三方统一拼图（T016 SDK 阶段细化）。 */
function appIcon(app: AppEntry): typeof Globe {
  if (app.official) {
    if (app.contribution_id === 'browser') return Globe;
    if (app.contribution_id === 'terminal') return TerminalSquare;
    if (app.contribution_id === 'agent-team') return Bot;
  }
  return Puzzle;
}

export function ExtensionMatrix({
  onOpenApp,
  runningKinds = [],
  runningPluginApps = [],
  onNewAppTab,
  onCloseApp,
  onOpenPluginApp,
}: ExtensionMatrixProps) {
  const [apps, setApps] = useState<AppEntry[]>([]);

  useEffect(() => {
    let active = true;
    api.listExtensionApps()
      .then((entries) => {
        if (active) setApps(entries);
      })
      .catch(() => {
        if (active) setApps([]);
      });
    return () => {
      active = false;
    };
  }, []);

  return (
    <div className="grid min-h-0 flex-1 auto-rows-min grid-cols-6 place-content-start gap-x-1.5 gap-y-2 overflow-y-auto p-3 lg:grid-cols-8">
      {apps.map((app) => {
        const Icon = appIcon(app);
        const appKey = `${app.plugin_id}:${app.contribution_id}`;
        const officialBuiltinKind: TabKind | null =
          app.official && (app.contribution_id === 'browser' || app.contribution_id === 'terminal')
            ? app.contribution_id
            : null;
        const running = officialBuiltinKind
          ? runningKinds.includes(officialBuiltinKind)
          : runningPluginApps.includes(appKey);
        const multi = officialBuiltinKind
          ? BUILTIN_TAB_KIND_MULTI[officialBuiltinKind]
          : app.open_mode === 'multi';
        const open = () => {
          if (officialBuiltinKind) {
            onOpenApp(officialBuiltinKind);
          } else {
            onOpenPluginApp?.(app);
          }
        };
        return (
          <ContextMenu key={appKey}>
            <ContextMenuTrigger asChild>
              <button
                type="button"
                onClick={open}
                className="group flex w-full cursor-default flex-col items-center gap-1"
                title={`${app.title}${running ? '（已打开）' : ''}（${multi ? '多实例' : '单实例'}）—— ${app.description}`}
              >
                <span className="relative flex h-12 w-12 items-center justify-center rounded-lg border bg-muted/50 transition-colors group-hover:border-primary/50 group-hover:bg-accent">
                  <Icon className="h-6 w-6 text-muted-foreground group-hover:text-foreground" />
                  {running && (
                    <span
                      className="absolute -right-1 -top-1 h-2.5 w-2.5 rounded-full bg-emerald-500 ring-2 ring-background"
                      title="已打开"
                      aria-label={`${app.title}已打开`}
                    />
                  )}
                </span>
                <span className="max-w-full truncate text-[11px] text-foreground/90">
                  {app.title}
                </span>
              </button>
            </ContextMenuTrigger>
            <ContextMenuContent>
              <ContextMenuItem onClick={open}>
                {running ? `聚焦${app.title}` : `打开${app.title}`}
              </ContextMenuItem>
              {multi && officialBuiltinKind && onNewAppTab && (
                <ContextMenuItem onClick={() => onNewAppTab(officialBuiltinKind)}>
                  新建{app.title}标签页
                </ContextMenuItem>
              )}
              {running && officialBuiltinKind && onCloseApp && (
                <>
                  <ContextMenuSeparator className="my-1" />
                  <ContextMenuItem onClick={() => onCloseApp(officialBuiltinKind)}>
                    关闭{multi ? `全部${TAB_KIND_NAME[officialBuiltinKind]}标签页` : `${app.title}`}
                  </ContextMenuItem>
                </>
              )}
            </ContextMenuContent>
          </ContextMenu>
        );
      })}
    </div>
  );
}
