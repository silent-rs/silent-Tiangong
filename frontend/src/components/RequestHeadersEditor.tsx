import { Info, Plus, Trash2 } from "lucide-react";
import { Portal as TooltipPortal } from "@radix-ui/react-tooltip";
import { Input } from "./ui/input";
import { Button } from "./ui/button";
import { Label } from "./ui/label";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "./ui/tooltip";

export function RequestHeadersEditor({ headers = {}, onChange }: {
  headers?: Record<string, string>;
  onChange: (headers: Record<string, string>) => void;
}) {
  const entries = Object.entries(headers);
  const update = (index: number, name: string, value: string) => {
    const next = [...entries];
    next[index] = [name, value];
    onChange(Object.fromEntries(next));
  };
  const add = () => {
    let name = 'X-Header';
    for (let i = 1; name in headers; i++) name = `X-Header-${i}`;
    onChange({ ...headers, [name]: '' });
  };
  return <div className="space-y-2">
    <div className="flex items-center justify-between gap-2">
      <div className="flex items-center gap-1">
        <Label className="text-xs">请求头</Label>
        <TooltipProvider delayDuration={150}>
          <Tooltip>
            <TooltipTrigger asChild>
              <button type="button" aria-label="请求头帮助" className="rounded p-1 text-muted-foreground hover:text-foreground focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"><Info className="h-3.5 w-3.5" /></button>
            </TooltipTrigger>
            <TooltipPortal>
              <TooltipContent className="z-[90] max-w-72 text-xs" side="top">
                请求头的值可以使用 <code className="font-mono">{'${session_id}'}</code>。发送对话请求时会替换为当前会话 ID，同一会话内保持一致。
              </TooltipContent>
            </TooltipPortal>
          </Tooltip>
        </TooltipProvider>
      </div>
      <div className="flex items-center gap-1">
        <Button type="button" size="icon" variant="ghost" className="h-7 w-7" onClick={add} title="添加请求头" aria-label="添加请求头"><Plus className="h-3.5 w-3.5" /></Button>
      </div>
    </div>
    {entries.map(([name, value], index) => <div key={index} className="flex flex-wrap gap-2 sm:flex-nowrap">
      <Input aria-label={`请求头名称 ${index + 1}`} value={name} onChange={(event) => update(index, event.target.value, value)} className="h-8 min-w-0 flex-1 text-xs font-mono" placeholder="Header-Name" />
      <Input aria-label={`请求头值 ${index + 1}`} value={value} onChange={(event) => update(index, name, event.target.value)} className="h-8 min-w-0 flex-[2] text-xs font-mono" placeholder="${session_id}" />
      <Button type="button" size="icon" variant="ghost" className="h-8 w-8 shrink-0" title="删除请求头" aria-label={`删除请求头 ${index + 1}`} onClick={() => onChange(Object.fromEntries(entries.filter((_, i) => i !== index)))}><Trash2 className="h-3.5 w-3.5" /></Button>
    </div>)}
  </div>;
}
