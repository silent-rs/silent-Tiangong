import { ChevronRight } from "lucide-react";
import type { MessageItem } from "./types";
import { cacheHitRate, sumUsage } from "./usage";

const number = new Intl.NumberFormat("zh-CN");
const rateText = (rate: number | null) => rate == null ? "未知" : `${(rate * 100).toFixed(1)}%`;
const statuses = { success: "完成", failed: "失败", cancelled: "中断" };

export function CallUsageDetails({ messages }: { messages: MessageItem[] }) {
  const calls = messages.filter((message) => message.usage != null);
  if (calls.length === 0) return null;
  const usage = sumUsage(calls.map((message) => message.usage!));
  return (
    <details className="group min-w-0 text-[11px] text-muted-foreground tabular-nums">
      <summary className="flex cursor-pointer list-none flex-wrap items-center gap-x-3 gap-y-1 py-1 [&::-webkit-details-marker]:hidden">
        <span className="inline-flex items-center gap-1"><ChevronRight className="h-3 w-3 group-open:rotate-90" />调用用量 · {calls.length} 次</span>
        <span>输入 {number.format(usage.prompt_tokens)}</span>
        <span>输出 {number.format(usage.completion_tokens)}</span>
        <span>缓存命中 {rateText(cacheHitRate(usage))}</span>
      </summary>
      <div className="max-w-full overflow-x-auto">
        <table className="w-full text-left">
          <thead><tr className="border-b border-border/60">
            {["模型 / 调用", "状态", "输入", "输出", "缓存命中", "命中率"].map((label) => <th key={label} className="px-2 py-1 font-medium whitespace-nowrap">{label}</th>)}
          </tr></thead>
          <tbody>{calls.map((message) => {
            const entry = message.usage!;
            return <tr key={message.id} className="border-b border-border/30">
              <td className="max-w-48 break-all px-2 py-1" title={message.created_at}>{entry.model}<span className="block whitespace-nowrap opacity-70">{entry.source === "context_summary" ? "压缩" : "对话"}</span></td>
              <td className="whitespace-nowrap px-2 py-1">{statuses[entry.status]}</td>
              <td className="px-2 py-1">{number.format(entry.prompt_tokens)}</td>
              <td className="px-2 py-1">{number.format(entry.completion_tokens)}</td>
              <td className="px-2 py-1">{entry.prompt_cache_hit_tokens == null ? "未知" : number.format(entry.prompt_cache_hit_tokens)}</td>
              <td className="px-2 py-1">{rateText(cacheHitRate(entry))}</td>
            </tr>;
          })}</tbody>
        </table>
      </div>
    </details>
  );
}
