import { useStore } from '@/store/useStore';
import { Badge } from './ui/badge';
import { Button } from './ui/button';
import { CheckCircle, Clock, XCircle, Loader2, ChevronDown, ChevronRight, Square, Copy } from 'lucide-react';
import { useState } from 'react';

export function PlanPanel() {
  const { runStatus, currentPlan, cancelTurn } = useStore();
  const [expandedItems, setExpandedItems] = useState<Set<string>>(new Set());
  const [copiedStepId, setCopiedStepId] = useState<string | null>(null);
  const [isCancelling, setIsCancelling] = useState(false);

  if (!currentPlan || runStatus === 'idle' || runStatus === 'completed' || runStatus === 'failed') {
    return null;
  }

  const toggleItem = (itemId: string) => {
    setExpandedItems((prev) => {
      const next = new Set(prev);
      if (next.has(itemId)) {
        next.delete(itemId);
      } else {
        next.add(itemId);
      }
      return next;
    });
  };

  const expandAll = () => {
    const allIds = new Set(currentPlan.items.map(item => item.id));
    setExpandedItems(allIds);
  };

  const collapseAll = () => {
    setExpandedItems(new Set());
  };

  const handleCancel = async () => {
    setIsCancelling(true);
    try {
      await cancelTurn();
    } finally {
      setIsCancelling(false);
    }
  };

  const handleCopyStep = (stepDescription: string, stepId: string) => {
    navigator.clipboard.writeText(stepDescription).then(() => {
      setCopiedStepId(stepId);
      setTimeout(() => setCopiedStepId(null), 2000);
    });
  };

  const getStatusIcon = (status: string) => {
    switch (status) {
      case 'Completed':
        return <CheckCircle className="w-4 h-4 text-[#4CAF50]" />;
      case 'Failed':
        return <XCircle className="w-4 h-4 text-[#F44336]" />;
      case 'InProgress':
        return <Loader2 className="w-4 h-4 text-[#FFC107] animate-spin" />;
      default:
        return <Clock className="w-4 h-4 text-muted-foreground" />;
    }
  };

  const getStatusText = (status: string) => {
    switch (status) {
      case 'Completed':
        return '已完成';
      case 'Failed':
        return '失败';
      case 'InProgress':
        return '进行中';
      default:
        return '待执行';
    }
  };

  const getStatusColor = (status: string) => {
    switch (status) {
      case 'Completed':
        return 'text-[#4CAF50]';
      case 'Failed':
        return 'text-[#F44336]';
      case 'InProgress':
        return 'text-[#FFC107]';
      default:
        return 'text-muted-foreground';
    }
  };

  // 计算总体进度
  const totalSteps = currentPlan.items.reduce((sum, item) => sum + item.steps.length, 0);
  const completedSteps = currentPlan.items.reduce(
    (sum, item) => sum + item.steps.filter(s => s.status === 'Completed').length,
    0
  );
  const progressPercentage = totalSteps > 0 ? Math.round((completedSteps / totalSteps) * 100) : 0;

  return (
    <div className="border-t border-border bg-card">
      <div className="max-w-3xl mx-auto p-4">
        {/* 头部 */}
        <div className="flex items-center justify-between mb-3">
          <div className="flex items-center gap-2">
            <Clock className="w-4 h-4 text-[#FFC107]" />
            <h3 className="text-sm font-medium text-white">执行计划</h3>
            <Badge variant="outline" className="border-border text-muted-foreground">
              {runStatus === 'planning' ? '计划中' : '执行中'}
            </Badge>
            {/* 进度显示 */}
            {totalSteps > 0 && (
              <span className="text-xs text-muted-foreground">
                {completedSteps}/{totalSteps} ({progressPercentage}%)
              </span>
            )}
          </div>

          <div className="flex items-center gap-2">
            {/* 展开/折叠全部 */}
            <Button
              variant="ghost"
              size="sm"
              className="text-xs text-muted-foreground hover:text-white hover:bg-accent h-7"
              onClick={expandedItems.size === currentPlan.items.length ? collapseAll : expandAll}
            >
              {expandedItems.size === currentPlan.items.length ? '折叠全部' : '展开全部'}
            </Button>

            {/* 取消按钮 */}
            {runStatus !== 'idle' && (
              <Button
                variant="ghost"
                size="sm"
                className="text-xs text-red-400 hover:text-red-300 hover:bg-red-500/10 h-7"
                onClick={handleCancel}
                disabled={isCancelling}
              >
                <Square className="w-3 h-3 mr-1" />
                {isCancelling ? '取消中...' : '取消执行'}
              </Button>
            )}
          </div>
        </div>

        {/* 计划目标 */}
        <div className="text-xs text-muted-foreground mb-3">{currentPlan.objective}</div>

        {/* 进度条 */}
        {totalSteps > 0 && (
          <div className="mb-3">
            <div className="h-1.5 bg-background rounded-full overflow-hidden">
              <div
                className="h-full bg-primary transition-all duration-300 ease-out"
                style={{ width: `${progressPercentage}%` }}
              />
            </div>
          </div>
        )}

        {/* 计划项列表 */}
        <div className="space-y-2">
          {currentPlan.items.map((item, index) => {
            const isExpanded = expandedItems.has(item.id);
            const itemCompletedSteps = item.steps.filter((s) => s.status === 'Completed').length;
            const itemTotalSteps = item.steps.length;
            const itemProgress = itemTotalSteps > 0 ? Math.round((itemCompletedSteps / itemTotalSteps) * 100) : 0;

            return (
              <div key={item.id} className="bg-background rounded-lg border border-border overflow-hidden">
                {/* 计划项标题 */}
                <button
                  className="w-full px-3 py-2.5 flex items-center gap-2 text-left hover:bg-accent transition-colors"
                  onClick={() => toggleItem(item.id)}
                >
                  {/* 序号 */}
                  <span className="text-xs text-muted-foreground font-mono w-5 flex-shrink-0">
                    {index + 1}.
                  </span>

                  {/* 展开/折叠图标 */}
                  {isExpanded ? (
                    <ChevronDown className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                  ) : (
                    <ChevronRight className="w-4 h-4 text-muted-foreground flex-shrink-0" />
                  )}

                  {/* 状态图标 */}
                  {getStatusIcon(item.status)}

                  {/* 描述 */}
                  <span className="flex-1 text-sm text-[#F3F4F6] truncate">{item.description}</span>

                  {/* 状态文本 */}
                  <span className={`text-xs ${getStatusColor(item.status)} flex-shrink-0`}>
                    {getStatusText(item.status)}
                  </span>

                  {/* 步骤进度 */}
                  {itemTotalSteps > 0 && (
                    <span className="text-xs text-muted-foreground flex-shrink-0">
                      {itemCompletedSteps}/{itemTotalSteps}
                    </span>
                  )}
                </button>

                {/* 计划项进度条 */}
                {itemTotalSteps > 0 && (
                  <div className="px-3 pb-2">
                    <div className="h-0.5 bg-card rounded-full overflow-hidden">
                      <div
                        className="h-full bg-primary transition-all duration-300 ease-out"
                        style={{ width: `${itemProgress}%` }}
                      />
                    </div>
                  </div>
                )}

                {/* 展开的步骤列表 */}
                {isExpanded && item.steps.length > 0 && (
                  <div className="px-3 pb-3 pl-14 space-y-1.5 border-t border-[#2A2D2E] pt-2">
                    {item.steps.map((step, stepIndex) => {
                      const isCopied = copiedStepId === step.id;

                      return (
                        <div
                          key={step.id}
                          className="group flex items-start gap-2 text-sm py-1 px-2 rounded hover:bg-card transition-colors"
                        >
                          {/* 步骤序号 */}
                          <span className="text-xs text-muted-foreground font-mono mt-0.5 flex-shrink-0">
                            {stepIndex + 1}
                          </span>

                          {/* 状态图标 */}
                          <div className="flex-shrink-0 mt-0.5">{getStatusIcon(step.status)}</div>

                          {/* 步骤描述 */}
                          <div className="flex-1 min-w-0">
                            <p className="text-muted-foreground break-words">{step.description}</p>
                            {step.source === 'Dynamic' && (
                              <Badge variant="outline" className="mt-1 text-xs border-border text-muted-foreground">
                                动态生成
                              </Badge>
                            )}
                          </div>

                          {/* 复制按钮 */}
                          <Button
                            variant="ghost"
                            size="icon"
                            className="opacity-0 group-hover:opacity-100 flex-shrink-0 h-6 w-6 hover:bg-accent"
                            onClick={() => handleCopyStep(step.description, step.id)}
                            title="复制步骤内容"
                          >
                            {isCopied ? (
                              <CheckCircle className="w-3.5 h-3.5 text-[#4CAF50]" />
                            ) : (
                              <Copy className="w-3.5 h-3.5 text-muted-foreground" />
                            )}
                          </Button>
                        </div>
                      );
                    })}
                  </div>
                )}
              </div>
            );
          })}
        </div>

        {/* 空状态 */}
        {currentPlan.items.length === 0 && (
          <div className="text-center text-muted-foreground py-8">
            暂无计划项
          </div>
        )}
      </div>
    </div>
  );
}
