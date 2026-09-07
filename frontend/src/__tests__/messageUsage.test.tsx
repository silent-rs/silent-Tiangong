import { describe, expect, it } from 'vitest';
import { renderToStaticMarkup } from 'react-dom/server';
import type { MessageUsage, TokenUsage } from '@/api/tauri';
import { cacheHitRate, sumUsage } from '@/components/message/usage';
import { CallUsageDetails } from '@/components/message/CallUsageDetails';
import type { MessageItem } from '@/components/message/types';

function usage(input: number, hit: number | null): TokenUsage {
  return { prompt_tokens: input, completion_tokens: 10, total_tokens: input + 10,
    prompt_cache_hit_tokens: hit, prompt_cache_miss_tokens: hit == null ? null : input - hit };
}

function message(id: string, tokens: TokenUsage): MessageItem {
  const call: MessageUsage = { ...tokens, model: 'test-model', agent_id: 'agent-1', turn_id: 'turn-1', source: 'react', status: 'success' };
  return { id, role: 'assistant', content: [{ type: 'text', text: '回复' }], reasoning_content: '', created_at: new Date().toISOString(), usage: call };
}

describe('模型调用用量', () => {
  it('按输入量加权计算汇总命中率', () => {
    const total = sumUsage([usage(100, 100), usage(900, 0)]);
    expect(total.prompt_tokens).toBe(1000);
    expect(total.total_tokens).toBe(1020);
    expect(cacheHitRate(total)).toBe(0.1);
  });

  it('区分零命中、缺失和不完整统计', () => {
    expect(cacheHitRate(usage(100, 0))).toBe(0);
    expect(cacheHitRate(usage(100, null))).toBeNull();
    expect(cacheHitRate(usage(0, 0))).toBeNull();
    expect(cacheHitRate(sumUsage([usage(100, 80), usage(900, null)]))).toBeNull();
  });

  it('重新加载消息后仍能展示调用明细和加权汇总', () => {
    const messages: MessageItem[] = JSON.parse(JSON.stringify([message('a', usage(100, 100)), message('b', usage(900, 0))]));
    const html = renderToStaticMarkup(<CallUsageDetails messages={messages} />);
    expect(html).toContain('2');
    expect(html).toContain('1,000');
    expect(html).toContain('10.0%');
    expect(html).toContain('100.0%');
    expect(html).toContain('0.0%');
    expect(html).toContain('test-model');
  });

  it('旧消息不补造用量，未知缓存数据不展示为零', () => {
    const old = message('old', usage(100, 80));
    delete old.usage;
    expect(renderToStaticMarkup(<CallUsageDetails messages={[old]} />)).toBe('');
    const html = renderToStaticMarkup(<CallUsageDetails messages={[message('new', usage(100, null))]} />);
    expect(html).toContain('未知');
    expect(html).not.toContain('0.0%');
  });
});
