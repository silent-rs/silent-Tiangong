import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { MdPreview } from 'md-editor-rt';
import { setupMarkdownLinkify } from '@/utils/markdownLinkify';

setupMarkdownLinkify();

describe('链接识别修正', () => {
  let container: HTMLDivElement;
  let root: Root;

  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement('div');
    document.body.appendChild(container);
    root = createRoot(container);
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  async function renderMd(text: string) {
    await act(async () => {
      root.render(<MdPreview modelValue={text} theme="light" previewTheme="github" />);
    });
    await act(async () => { await new Promise((r) => setTimeout(r, 50)); });
  }

  it('带后缀的文件名不被识别为链接', async () => {
    await renderMd('先改 slots.rs，再同步 manifest.rs 和 session.input-status');
    expect(container.querySelector('a')).toBeNull();
    expect(container.textContent).toContain('slots.rs');
    expect(container.textContent).toContain('session.input-status');
  });

  it('完整 URL 仍然识别为链接，尾部星号剥离并参与加粗', async () => {
    await renderMd('详见 **https://github.com/silent-rs/silent-Tiangong/pull/543**');
    const link = container.querySelector('a');
    expect(link).not.toBeNull();
    expect(link!.getAttribute('href')).toBe('https://github.com/silent-rs/silent-Tiangong/pull/543');
    expect(link!.textContent).not.toContain('*');
    expect(container.querySelector('strong')).not.toBeNull();
  });

  it('URL 后直接跟下划线加粗同样剥离', async () => {
    await renderMd('入口 __https://example.com/book__ 收尾');
    const link = container.querySelector('a');
    expect(link).not.toBeNull();
    expect(link!.getAttribute('href')).toBe('https://example.com/book');
    expect(link!.textContent).not.toContain('_');
  });

  it('普通 URL 与邮箱识别不受影响', async () => {
    await renderMd('访问 https://example.com/path?x=1 联系 foo@example.com');
    const links = container.querySelectorAll('a');
    expect(links.length).toBeGreaterThanOrEqual(2);
    expect(links[0].getAttribute('href')).toBe('https://example.com/path?x=1');
    expect([...links].some((l) => l.getAttribute('href')?.includes('foo@example.com'))).toBe(true);
  });

  it('http 前缀的 URL 正常识别为链接', async () => {
    await renderMd('旧的入口 http://example.com/legacy 仍然有效');
    const link = container.querySelector('a');
    expect(link).not.toBeNull();
    expect(link!.getAttribute('href')).toBe('http://example.com/legacy');
    expect(link!.textContent).toBe('http://example.com/legacy');
  });

  it('URL 尾部句号按既有语义剥离', async () => {
    await renderMd('见 https://example.com/page. 结束');
    const link = container.querySelector('a');
    expect(link).not.toBeNull();
    expect(link!.getAttribute('href')).toBe('https://example.com/page');
  });
});
