import { useState } from 'react';
import { Terminal, CheckCircle2, XCircle, Loader2 } from 'lucide-react';

export interface ToolCall {
  id: string;
  name: string;
  arguments?: Record<string, any>;
  result?: string;
  status: 'pending' | 'running' | 'success' | 'error';
  duration?: number;
}

interface ToolCallBlockProps {
  toolCalls: ToolCall[];
}

/**
 * 工具参数 / 结果在前端截取的最大字符数，避免超长输出卡顿 UI。
 */
const TOOL_FIELD_PREVIEW_LIMIT = 2000;

/** 工具参数 / 结果渲染：超长内容截取为预览，点击展开全部回看完整内容。 */
function ToolField({ label, value }: { label: string; value: string }) {
  const [showAll, setShowAll] = useState(false);
  const overLimit = value.length > TOOL_FIELD_PREVIEW_LIMIT;
  const preview = overLimit && !showAll ? value.slice(0, TOOL_FIELD_PREVIEW_LIMIT) : value;
  return (
    <div>
      <div className="text-xs text-[#6B7280] mb-1">{label}：</div>
      <pre className="text-xs font-mono text-[#94A3B8] bg-[#0D0D1A] rounded p-2 overflow-x-auto max-h-64 overflow-y-auto whitespace-pre-wrap break-words">
        {preview}
      </pre>
      {overLimit && (
        <button
          type="button"
          onClick={() => setShowAll((v) => !v)}
          className="text-xs text-[#6366F1] hover:underline mt-1"
        >
          {showAll
            ? '收起'
            : `展开全部（${(value.length / 1000).toFixed(1)}k 字符，已截取前 ${(TOOL_FIELD_PREVIEW_LIMIT / 1000).toFixed(0)}k）`}
        </button>
      )}
    </div>
  );
}

export function ToolCallBlock({ toolCalls }: ToolCallBlockProps) {
  if (toolCalls.length === 0) {
    return null;
  }

  const getStatusIcon = (status: ToolCall['status']) => {
    switch (status) {
      case 'pending':
        return <Loader2 className="w-3.5 h-3.5 text-[#FFC107] animate-spin" />;
      case 'running':
        return <Loader2 className="w-3.5 h-3.5 text-[#10A37F] animate-spin" />;
      case 'success':
        return <CheckCircle2 className="w-3.5 h-3.5 text-[#10A37F]" />;
      case 'error':
        return <XCircle className="w-3.5 h-3.5 text-[#EF4444]" />;
    }
  };

  const getStatusText = (status: ToolCall['status']) => {
    switch (status) {
      case 'pending':
        return '等待中';
      case 'running':
        return '执行中';
      case 'success':
        return '已完成';
      case 'error':
        return '失败';
    }
  };

  const getStatusColor = (status: ToolCall['status']) => {
    switch (status) {
      case 'pending':
        return 'text-[#FFC107]';
      case 'running':
        return 'text-[#10A37F]';
      case 'success':
        return 'text-[#10A37F]';
      case 'error':
        return 'text-[#EF4444]';
    }
  };

  return (
    <div className="mt-3 space-y-2">
      {toolCalls.map((toolCall) => (
        <div
          key={toolCall.id}
          className="rounded-md bg-[#1A1A2E] border border-[#2D2D4A] overflow-hidden"
        >
          {/* 标题栏 */}
          <div className="px-3 py-2 flex items-center justify-between bg-[#12121F]">
            <div className="flex items-center gap-2">
              <Terminal className="w-3.5 h-3.5 text-[#6366F1]" />
              <span className="text-xs font-mono font-medium text-[#E0E7FF]">
                {toolCall.name}
              </span>
            </div>
            <div className="flex items-center gap-2">
              {toolCall.duration && (
                <span className="text-xs text-[#6B7280] font-mono">
                  {toolCall.duration}ms
                </span>
              )}
              <div className="flex items-center gap-1">
                {getStatusIcon(toolCall.status)}
                <span className={`text-xs font-medium ${getStatusColor(toolCall.status)}`}>
                  {getStatusText(toolCall.status)}
                </span>
              </div>
            </div>
          </div>

          {/* 参数和结果 */}
          <div className="px-3 py-2 space-y-2">
            {toolCall.arguments && Object.keys(toolCall.arguments).length > 0 && (
              <ToolField label="参数" value={JSON.stringify(toolCall.arguments, null, 2)} />
            )}

            {toolCall.result && (
              <ToolField
                label="结果"
                value={typeof toolCall.result === 'string'
                  ? toolCall.result
                  : JSON.stringify(toolCall.result, null, 2)}
              />
            )}
          </div>
        </div>
      ))}
    </div>
  );
}
