import { Globe, Grid3x3, TerminalSquare } from 'lucide-react';
import type { TabKind } from '@/api/tauri';

/**
 * 拓展区 App 矩阵（矩阵态视图骨架）。
 *
 * 启动台风格：图标块（约系统 App 图标尺寸）+ 下方名称 + 打开模式角标，
 * 用途说明放悬浮提示。T008 交付官方内置 App（浏览器/终端）入口，保证入口
 * 收敛后原有能力不回退；T009 接入三方 App（listExtensionApps）、已打开标识
 * 与运行态，T010 接入三方 App 实例打开。
 */
export interface ExtensionMatrixProps {
  /** 打开官方内置 App（进入 App 态）。 */
  onOpenApp: (kind: TabKind) => void;
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

export function ExtensionMatrix({ onOpenApp }: ExtensionMatrixProps) {
  return (
    <div className="flex h-full flex-1 flex-col bg-background">
      <div className="flex shrink-0 items-center gap-2 border-b px-3 py-1.5 text-xs text-muted-foreground">
        <Grid3x3 className="h-3.5 w-3.5" />
        拓展区
      </div>
      <div className="grid min-h-0 flex-1 auto-rows-min grid-cols-4 place-content-start gap-x-3 gap-y-4 overflow-y-auto p-5 lg:grid-cols-5">
        {OFFICIAL_APPS.map((app) => {
          const Icon = app.icon;
          return (
            <button
              key={app.kind}
              type="button"
              onClick={() => onOpenApp(app.kind)}
              className="group flex w-full flex-col items-center gap-1.5"
              title={`${app.name}（${app.openMode}）—— ${app.description}`}
            >
              <span className="relative flex h-14 w-14 items-center justify-center rounded-xl border bg-muted/50 transition-colors group-hover:border-primary/50 group-hover:bg-accent">
                <Icon className="h-7 w-7 text-muted-foreground group-hover:text-foreground" />
                <span
                  className="absolute -right-1.5 -top-1.5 rounded-full bg-background px-1 py-px text-[9px] leading-none text-muted-foreground ring-1 ring-border"
                  aria-label={`打开模式：${app.openMode}`}
                >
                  {app.openMode === '多实例' ? '多' : '单'}
                </span>
              </span>
              <span className="max-w-full truncate text-xs text-foreground/90">
                {app.name}
              </span>
            </button>
          );
        })}
      </div>
    </div>
  );
}
