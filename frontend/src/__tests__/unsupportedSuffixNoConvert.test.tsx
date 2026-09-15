import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { MdPreview } from 'md-editor-rt';
import { setupMarkdownLinkify } from '@/utils/markdownLinkify';
import { resolveMarkdownImages } from '@/components/message/utils';

setupMarkdownLinkify();

const MD = `放行 file: 协议 — [frontend/src/utils/markdownLinkify.ts:55](/Users/hubertshelley/Documents/silent/tiangong/frontend/src/utils/markdownLinkify.ts)
包装默认 \`validateLink\`，只额外放行 \`file:\`，其余危险协议仍由默认实现拒绝。

本地文件链接高亮 — [frontend/src/utils/markdownLinkify.ts:62](/Users/hubertshelley/Documents/silent/tiangong/frontend/src/utils/markdownLinkify.ts) + [frontend/src/index.css:205](/Users/hubertshelley/Documents/silent/tiangong/frontend/src/index.css)
链式包装 \`link_open\` 渲染规则。

Windows 路径兼容 — [frontend/src/components/message/utils.ts:134](/Users/hubertshelley/Documents/silent/tiangong/frontend/src/components/message/utils.ts)
\`toFileUrl\` 原来只在「无 file: 前缀」分支做反斜杠转换。`;

describe('不可渲染后缀的本地路径链接不转换', () => {
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

  it('.ts/.css 本地路径链接退化为纯文本', async () => {
    await act(async () => {
      root.render(<MdPreview modelValue={MD} theme="light" previewTheme="github" />);
    });
    await act(async () => { await new Promise((r) => setTimeout(r, 50)); });

    const links = [...container.querySelectorAll('a')];
    console.log('LINKS:', links.length, links.map((l) => ({ href: l.getAttribute('href'), cls: l.className })));
    expect(links.length).toBe(0);
    expect(container.textContent).toContain('markdownLinkify.ts:55');
  });

  it('经消息预处理链后同样不转换', async () => {
    await act(async () => {
      root.render(<MdPreview modelValue={resolveMarkdownImages(MD)} theme="light" previewTheme="github" />);
    });
    await act(async () => { await new Promise((r) => setTimeout(r, 50)); });

    const links = [...container.querySelectorAll('a')];
    expect(links.length).toBe(0);
  });
});
