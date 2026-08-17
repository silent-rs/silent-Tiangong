import { Globe, TerminalSquare } from 'lucide-react';
import type { TabKind } from '@/api/tauri';

/**
 * 拓展区 App 矩阵（矩阵态视图骨架）。
 *
 * 启动台风格：图标块 + 下方名称，从顶部起排、等距紧凑网格。
 * 图标右上角预留给运行态状态点（T009 接入，与「App 使用中」标识共用位置），
 * 打开模式等说明放悬浮提示。T008 交付官方内置 App（浏览器/终端）入口；
 * T009 接入三方 App（listExtensionApps）与已打开标识，T010 接入实例打开。
 */
export interface ExtensionMatrixProps {
  /** 打开官方内置 App（进入 App 态）。 */
  onOpenApp: (kind: TabKind) => void;
  /** 当前会话存在已打开实例的官方 App（图标右上角显示「在用」绿点）。 */
  runningKinds?: TabKind[];
}

const OFFICIAL_APPS: Array<{
  kind: TabKind;
  name: string;
  description: string;
  openMode: string;
  icon: typeof Globe;
}> = [
  {
    kind: 'browser',
    name: '浏览器',
    description: '嵌入式浏览器，Agent 与你共用同一页面',
    openMode: '单实例',
    icon: Globe,
  },
  {
    kind: 'terminal',
    name: '终端',
    description: '嵌入式终端，支持多标签与会话隔离',
    openMode: '多实例',
    icon: TerminalSquare,
  },
];

export function ExtensionMatrix({ onOpenApp, runningKinds = [] }: ExtensionMatrixProps) {
  return (
    <div className="grid min-h-0 flex-1 auto-rows-min grid-cols-6 place-content-start gap-x-1.5 gap-y-2 overflow-y-auto p-3 lg:grid-cols-8">
        {OFFICIAL_APPS.map((app) => {
          const Icon = app.icon;
          const running = runningKinds.includes(app.kind);
          return (
            <button
              key={app.kind}
              type="button"
              onClick={() => onOpenApp(app.kind)}
              className="group flex w-full flex-col items-center gap-1"
              title={`${app.name}${running ? '（已打开）' : ''}（${app.openMode}）—— ${app.description}`}
            >
              <span className="relative flex h-12 w-12 items-center justify-center rounded-lg border bg-muted/50 transition-colors group-hover:border-primary/50 group-hover:bg-accent">
                <Icon className="h-6 w-6 text-muted-foreground group-hover:text-foreground" />
                {running && (
                  <span
                    className="absolute -right-1 -top-1 h-2.5 w-2.5 rounded-full bg-emerald-500 ring-2 ring-background"
                    title="已打开"
                    aria-label={`${app.name}已打开`}
                  />
                )}
              </span>
              <span className="max-w-full truncate text-[11px] text-foreground/90">
                {app.name}
              </span>
            </button>
          );
        })}
    </div>
  );
}
