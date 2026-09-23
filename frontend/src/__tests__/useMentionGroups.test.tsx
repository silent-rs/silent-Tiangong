import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useMentionGroups } from '@/hooks/useMentionGroups';
import { api } from '@/api/tauri';
import type { MentionTarget } from '@/api/tauri';

vi.mock('@/api/tauri', () => ({
  api: { getMentionGroups: vi.fn() },
}));

const mockedGet = vi.mocked(api.getMentionGroups);

let container: HTMLDivElement | null = null;
let root: Root | null = null;

type Props = { target: MentionTarget; query: string; active: boolean };

const state: { current: ReturnType<typeof useMentionGroups> } = { current: [] };

function Probe({ target, query, active }: Props) {
  state.current = useMentionGroups(target, query, active);
  return null;
}

async function render(props: Props) {
  await act(async () => {
    root!.render(<Probe {...props} />);
  });
}

function group(value: string) {
  return [{ kind: 'skill', label: '技能', candidates: [{ value, label: value, kind: 'skill', hint: '' }] }];
}

describe('useMentionGroups 迟到响应与目标切换', () => {
  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    vi.clearAllMocks();
    container = document.createElement('div');
    document.body.appendChild(container);
    root = createRoot(container);
    state.current = [];
  });

  afterEach(() => {
    act(() => root?.unmount());
    root = null;
    container?.remove();
    container = null;
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  it('查询词变化后重新请求，先前请求的迟到响应不覆盖新结果', async () => {
    let releaseFirst: (value: unknown) => void = () => {};
    mockedGet.mockImplementationOnce(
      () => new Promise((resolve) => { releaseFirst = resolve; }) as never,
    );
    mockedGet.mockImplementation(() => Promise.resolve(group('@skill:new')) as never);

    const target: MentionTarget = { kind: 'session', session_id: 's1' };
    await render({ target, query: '', active: true });
    // 等过防抖窗口，第一次请求已发出但未完成。
    await act(async () => { await new Promise((r) => setTimeout(r, 200)); });
    expect(mockedGet).toHaveBeenCalledTimes(1);

    // 查询词变化 → 第二次请求并先完成。
    await render({ target, query: 'ne', active: true });
    await act(async () => { await new Promise((r) => setTimeout(r, 200)); });
    expect(state.current[0]?.candidates[0]?.value).toBe('@skill:new');

    // 第一次请求的迟到响应到达，必须被丢弃。
    await act(async () => {
      releaseFirst(group('@skill:stale'));
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(state.current[0]?.candidates[0]?.value).toBe('@skill:new');
  });

  it('非活跃不请求；目标切换携带新上下文重新查询', async () => {
    mockedGet.mockResolvedValue(group('@skill:a') as never);

    await render({ target: { kind: 'global' }, query: '', active: false });
    await act(async () => { await new Promise((r) => setTimeout(r, 200)); });
    expect(mockedGet).not.toHaveBeenCalled();

    await render({ target: { kind: 'global' }, query: '', active: true });
    await act(async () => { await new Promise((r) => setTimeout(r, 200)); });
    expect(mockedGet).toHaveBeenCalledTimes(1);
    expect(mockedGet.mock.calls[0][2]).toMatchObject({ target: { kind: 'global' } });

    await render({ target: { kind: 'session', session_id: 's2' }, query: '', active: true });
    await act(async () => { await new Promise((r) => setTimeout(r, 200)); });
    expect(mockedGet).toHaveBeenCalledTimes(2);
    expect(mockedGet.mock.calls[1][2]).toMatchObject({
      target: { kind: 'session', session_id: 's2' },
      max_per_group: 1000,
    });
  });
});
