import type { MentionKind } from '@/utils/mentionBlocks';

interface MentionChipProps {
  kind: MentionKind;
  label: string;
  token?: string;
}

export const MENTION_MARK: Record<MentionKind, string> = {
  skill: 'S',
  mcp: 'M',
  agent: '@',
  all: '*',
};

export const MENTION_CHIP_BASE_CLASS =
  'mention-chip inline-flex max-w-[14rem] items-center gap-1 rounded border px-1.5 align-baseline text-xs leading-5 text-foreground';

export const MENTION_MARK_BASE_CLASS =
  'inline-flex h-4 min-w-4 shrink-0 items-center justify-center rounded-sm bg-background/80 px-0.5 text-[10px] font-semibold leading-none';

export const MENTION_LABEL_CLASS = 'min-w-0 truncate font-medium';

const MENTION_KIND_CLASS: Record<MentionKind, string> = {
  skill: 'border-amber-500/30 bg-amber-500/10',
  mcp: 'border-cyan-500/30 bg-cyan-500/10',
  agent: 'border-blue-500/30 bg-blue-500/10',
  all: 'border-rose-500/30 bg-rose-500/10',
};

const MENTION_MARK_KIND_CLASS: Record<MentionKind, string> = {
  skill: 'text-amber-700 dark:text-amber-300',
  mcp: 'text-cyan-700 dark:text-cyan-300',
  agent: 'text-blue-700 dark:text-blue-300',
  all: 'text-rose-700 dark:text-rose-300',
};

export function mentionChipClass(kind: MentionKind): string {
  return `${MENTION_CHIP_BASE_CLASS} ${MENTION_KIND_CLASS[kind]}`;
}

export function mentionMarkClass(kind: MentionKind): string {
  return `${MENTION_MARK_BASE_CLASS} ${MENTION_MARK_KIND_CLASS[kind]}`;
}

const TOKEN_PREFIX: Record<MentionKind, string> = {
  skill: '@skill:',
  mcp: '@mcp:',
  agent: '@',
  all: '@',
};

/**
 * @提及内联标签块（消息气泡展示用，React 渲染）。
 *
 * 输入框编辑器（MentionEditor）走 contenteditable 直建 DOM 路径，复用同一
 * 样式常量与标记，两条路径视觉一致。
 */
export function MentionChip({ kind, label, token }: MentionChipProps) {
  const rawToken = token ?? `${TOKEN_PREFIX[kind]}${label}`;
  return (
    <span
      className={mentionChipClass(kind)}
      title={rawToken}
      aria-label={rawToken}
      data-mention-token={token}
      data-mention-kind={kind}
    >
      <span className={mentionMarkClass(kind)} aria-hidden="true">{MENTION_MARK[kind]}</span>
      <span className={MENTION_LABEL_CLASS}>{label}</span>
    </span>
  );
}
