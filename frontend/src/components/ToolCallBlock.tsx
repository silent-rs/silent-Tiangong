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
              <div>
                <div className="text-xs text-[#6B7280] mb-1">参数：</div>
                <pre className="text-xs font-mono text-[#A5B4FC] bg-[#0D0D1A] rounded p-2 overflow-x-auto">
                  {JSON.stringify(toolCall.arguments, null, 2)}
                </pre>
              </div>
            )}

            {toolCall.result && (
              <div>
                <div className="text-xs text-[#6B7280] mb-1">结果：</div>
                <pre className="text-xs font-mono text-[#94A3B8] bg-[#0D0D1A] rounded p-2 overflow-x-auto max-h-32 overflow-y-auto">
                  {typeof toolCall.result === 'string'
                    ? toolCall.result
                    : JSON.stringify(toolCall.result, null, 2)}
                </pre>
              </div>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}
