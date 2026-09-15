import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it } from 'vitest';

import { MdPreview } from 'md-editor-rt';
import 'md-editor-rt/lib/preview.css';

describe('真实 MdPreview 单换行渲染', () => {
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

  it('单换行渲染为 <br>，markdown 语法生效', async () => {
    await act(async () => {
      root.render(<MdPreview modelValue={'第一行\n第二行\n\n**加粗** `代码`'} theme="light" previewTheme="github" />);
    });
    await act(async () => { await new Promise((r) => setTimeout(r, 50)); });

    const html = container.innerHTML;
    expect(container.querySelector('br')).not.toBeNull();
    expect(html).toContain('第一行');
    expect(container.querySelector('strong, b')).not.toBeNull();
    expect(container.querySelector('code')).not.toBeNull();
  });
});
