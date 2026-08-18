import { useState } from 'react';
import { api } from '@/api/tauri';
import { CheckCircle2, HelpCircle, ListChecks, ShieldCheck, ShieldX } from 'lucide-react';

/**
 * 交互接缝默认 UI（request_user 阻塞等待时渲染）：
 * approval（仅本次/本次运行内/拒绝）、confirm、choice、multi_choice、input、form。
 * 用户提交经 resolve_interaction 回传（注册表原子闭合，多界面唯一胜者）；
 * 收到 interaction_closed 后由 store 清空本卡片（迟到提交被拒绝）。
 */

interface FormField {
  key: string;
  label: string;
  type: 'string' | 'number' | 'boolean' | 'select';
  options?: string[];
}

export interface PendingInteraction {
  request_id: string;
  kind: string;
  title: string;
  description: string;
  payload: string;
}

export function InteractionCard({ interaction }: { interaction: PendingInteraction }) {
  const [formValues, setFormValues] = useState<Record<string, string>>({});
  const [selected, setSelected] = useState<string[]>([]);

  const respond = (resultJson: string) => {
    api.resolveInteraction(interaction.request_id, resultJson).catch(console.error);
  };

  const options = safeParse<string[]>(interaction.payload, []);
  const fields = safeParse<FormField[]>(interaction.payload, []);

  const toggleChoice = (option: string) => {
    setSelected((current) => {
      if (interaction.kind === 'multi_choice') {
        return current.includes(option)
          ? current.filter((item) => item !== option)
          : [...current, option];
      }
      return [option];
    });
  };

  return (
    <div className="flex justify-start">
      <div className="text-foreground max-w-[100%]">
        <div className="flex items-center gap-1.5 text-sm font-medium mb-1">
          <HelpCircle className="w-4 h-4 text-muted-foreground" />
          {interaction.title}
        </div>
        {interaction.description && (
          <div className="text-xs text-muted-foreground mb-2">{interaction.description}</div>
        )}

        {(interaction.kind === 'choice' || interaction.kind === 'multi_choice') && (
          <div className="space-y-1.5 mb-3">
            {options.map((option) => (
              <button
                key={option}
                type="button"
                onClick={() => toggleChoice(option)}
                className={`flex w-full max-w-md items-center gap-2 rounded-md border px-3 py-1.5 text-left text-xs transition-colors ${
                  selected.includes(option)
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
          <div className="text-xs text-muted-foreground mb-3">{interaction.payload}</div>
        )}

        {interaction.kind === 'input' && (
          <input
            type="text"
            className="w-full max-w-md rounded-md border bg-background px-2 py-1.5 text-xs mb-3"
            placeholder="请输入…"
            value={formValues.input ?? ''}
            onChange={(e) => setFormValues((values) => ({ ...values, input: e.target.value }))}
          />
        )}

        {interaction.kind === 'form' && (
          <div className="space-y-2 mb-3">
            {fields.map((field) => (
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
          {interaction.kind === 'approval' ? (
            <>
              <button
                type="button"
                className="flex items-center gap-1 px-3 py-1.5 rounded-md bg-green-600 hover:bg-green-700 text-white text-xs transition-colors"
                onClick={() => respond(JSON.stringify({ decision: 'approve_once' }))}
              >
                <ShieldCheck className="w-3.5 h-3.5" />
                仅本次允许
              </button>
              <button
                type="button"
                className="flex items-center gap-1 px-3 py-1.5 rounded-md bg-green-600/80 hover:bg-green-700/90 text-white text-xs transition-colors"
                title="本次运行内同工具不再询问"
                onClick={() => respond(JSON.stringify({ decision: 'approve_for_runtime' }))}
              >
                <ShieldCheck className="w-3.5 h-3.5" />
                本次运行内允许
              </button>
              <button
                type="button"
                className="flex items-center gap-1 px-3 py-1.5 rounded-md bg-destructive hover:bg-destructive/90 text-destructive-foreground text-xs transition-colors"
                onClick={() => respond(JSON.stringify({ decision: 'reject' }))}
              >
                <ShieldX className="w-3.5 h-3.5" />
                拒绝
              </button>
            </>
          ) : interaction.kind === 'confirm' ? (
            <>
              <button
                type="button"
                className="flex items-center gap-1 px-3 py-1.5 rounded-md bg-green-600 hover:bg-green-700 text-white text-xs transition-colors"
                onClick={() => respond('true')}
              >
                <CheckCircle2 className="w-3.5 h-3.5" />
                是
              </button>
              <button
                type="button"
                className="flex items-center gap-1 px-3 py-1.5 rounded-md bg-destructive hover:bg-destructive/90 text-destructive-foreground text-xs transition-colors"
                onClick={() => respond('false')}
              >
                <ShieldX className="w-3.5 h-3.5" />
                否
              </button>
            </>
          ) : (
            <button
              type="button"
              disabled={
                (interaction.kind === 'choice' || interaction.kind === 'multi_choice')
                  ? selected.length === 0
                  : false
              }
              className="flex items-center gap-1 px-3 py-1.5 rounded-md bg-green-600 hover:bg-green-700 disabled:opacity-50 text-white text-xs transition-colors"
              onClick={() => {
                if (interaction.kind === 'choice') {
                  respond(JSON.stringify(selected[0]));
                } else if (interaction.kind === 'multi_choice') {
                  respond(JSON.stringify(selected));
                } else if (interaction.kind === 'input') {
                  respond(JSON.stringify(formValues.input ?? ''));
                } else {
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
          )}
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
