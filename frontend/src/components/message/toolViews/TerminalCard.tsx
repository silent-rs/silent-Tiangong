import type { TerminalMaterial } from "../toolDisplayModel";
import { LongContent } from "./LongContent";

/**
 * 终端样式输出卡：命令行 + stdout/stderr 分区，黑底等宽，
 * 长输出在卡片内部滚动而不是撑爆消息流。
 */
export function TerminalCard({ terminal }: { terminal: TerminalMaterial }) {
  const { command, stdout, stderr } = terminal;
  if (!command && !stdout && !stderr) return null;
  return (
    <div className="rounded-md border border-border/60 bg-[#0D0D1A] overflow-hidden">
      {command && (
        <div className="px-3 py-1.5 border-b border-white/5 text-xs font-mono text-[#94A3B8] whitespace-pre-wrap break-all">
          <span className="text-[#6366F1] select-none">$ </span>
          {command}
        </div>
      )}
      {stdout && (
        <LongContent
          content={stdout}
          className="px-3 py-2 text-xs font-mono text-[#C9D1D9] whitespace-pre-wrap break-words max-h-72 overflow-y-auto"
        />
      )}
      {stderr && (
        <div className="border-t border-white/5">
          <LongContent
            content={stderr}
            className="px-3 py-2 text-xs font-mono text-[#F87171] whitespace-pre-wrap break-words max-h-48 overflow-y-auto"
          />
        </div>
      )}
    </div>
  );
}
