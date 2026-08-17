import { useState } from 'react';
import { api } from '@/api/tauri';
import { CheckCircle2, HelpCircle, ListChecks } from 'lucide-react';

/**
 * 交互接缝默认 UI（ask_user 挂起时渲染）：
 * - choice：候选列表点选
 * - form：简易字段表单（string/number/boolean/select）
 * - confirm：确认/取消
 * 用户提交经 respond_interaction 回传 Core 解锁工具；取消传 null（fail-closed 由
 * Core 侧超时/取消闭合兜底）。
 */

interface FormField {
  key: string;
  label: string;
  type: 'string' | 'number' | 'boolean' | 'select';
  options?: string[];
}

export function InteractionCard({
  interaction,
  onResponded,
}: {
  interaction: {
    interaction_id: string;
    kind: string;
    title: string;
    schema: string;
  };
  onResponded?: () => void;
}) {
  const [formValues, setFormValues] = useState<Record<string, string>>({});
  const [selected, setSelected] = useState<string | null>(null);

  const respond = (resultJson: string | null) => {
    api.respondInteraction(interaction.interaction_id, resultJson).catch(console.error);
    onResponded?.();
  };

  return (
    <div className="flex justify-start">
      <div className="text-foreground max-w-[100%]">
        <div className="flex items-center gap-1.5 text-sm font-medium mb-2">
          <HelpCircle className="w-4 h-4 text-muted-foreground" />
          {interaction.title}
        </div>

        {interaction.kind === 'choice' && (
          <div className="space-y-1.5 mb-3">
            {(safeParse<string[]>(interaction.schema, [])).map((option) => (
              <button
                key={option}
                type="button"
                onClick={() => setSelected(option)}
                className={`flex w-full max-w-md items-center gap-2 rounded-md border px-3 py-1.5 text-left text-xs transition-colors ${
                  selected === option
                    ? 'border-primary bg-accent text-accent-foreground'
                    : 'hover:bg-accent/50'
                }`}
              >
                <ListChecks className="w-3.5 h-3.5 shrink-0 text-muted-foreground" />
                {option}
              </button>
            ))}
          </div>
        )}

        {interaction.kind === 'confirm' && (
          <div className="text-xs text-muted-foreground mb-3">{interaction.schema}</div>
        )}

        {interaction.kind === 'form' && (
          <div className="space-y-2 mb-3">
            {safeParse<FormField[]>(interaction.schema, []).map((field) => (
              <label key={field.key} className="flex max-w-md items-center gap-2 text-xs">
                <span className="w-24 shrink-0 text-muted-foreground">{field.label}</span>
                {field.type === 'boolean' ? (
                  <input
                    type="checkbox"
                    checked={formValues[field.key] === 'true'}
                    onChange={(e) =>
                      setFormValues((values) => ({
                        ...values,
                        [field.key]: String(e.target.checked),
                      }))
                    }
                  />
                ) : field.type === 'select' ? (
                  <select
                    className="flex-1 rounded-md border bg-background px-2 py-1.5"
                    value={formValues[field.key] ?? ''}
                    onChange={(e) =>
                      setFormValues((values) => ({ ...values, [field.key]: e.target.value }))
                    }
                  >
                    <option value="" disabled>请选择</option>
                    {(field.options ?? []).map((option) => (
                      <option key={option} value={option}>{option}</option>
                    ))}
                  </select>
                ) : (
                  <input
                    type={field.type === 'number' ? 'number' : 'text'}
                    className="flex-1 rounded-md border bg-background px-2 py-1.5"
                    value={formValues[field.key] ?? ''}
                    onChange={(e) =>
                      setFormValues((values) => ({ ...values, [field.key]: e.target.value }))
                    }
                  />
                )}
              </label>
            ))}
          </div>
        )}

        <div className="flex items-center gap-2">
          <button
            type="button"
            disabled={interaction.kind === 'choice' && selected === null}
            className="flex items-center gap-1 px-3 py-1.5 rounded-md bg-green-600 hover:bg-green-700 disabled:opacity-50 text-white text-xs transition-colors"
            onClick={() => {
              if (interaction.kind === 'choice') {
                respond(JSON.stringify(selected));
              } else if (interaction.kind === 'confirm') {
                respond('true');
              } else {
                // form：按字段类型转换
                const fields = safeParse<FormField[]>(interaction.schema, []);
                const answers: Record<string, unknown> = {};
                for (const field of fields) {
                  const raw = formValues[field.key] ?? '';
                  if (field.type === 'number') {
                    answers[field.key] = Number(raw) || 0;
                  } else if (field.type === 'boolean') {
                    answers[field.key] = raw === 'true';
                  } else {
                    answers[field.key] = raw;
                  }
                }
                respond(JSON.stringify(answers));
              }
            }}
          >
            <CheckCircle2 className="w-3.5 h-3.5" />
            提交
          </button>
          <button
            type="button"
            className="flex items-center gap-1 px-3 py-1.5 rounded-md bg-destructive hover:bg-destructive/90 text-destructive-foreground text-xs transition-colors"
            onClick={() => {
              if (interaction.kind === 'confirm') {
                respond('false');
              } else {
                respond(null);
              }
            }}
          >
            {interaction.kind === 'confirm' ? '否' : '取消'}
          </button>
        </div>
      </div>
    </div>
  );
}

function safeParse<T>(text: string, fallback: T): T {
  try {
    return JSON.parse(text) as T;
  } catch {
    return fallback;
  }
}
