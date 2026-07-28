import {
  act,
  createRef,
  type ComponentProps,
  type RefObject,
} from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { MentionEditor, type MentionEditorHandle } from '@/components/MentionEditor';
import { UserMessageGroup } from '@/components/message/UserMessageGroup';
import type { MessageGroup } from '@/components/message/types';

let container: HTMLDivElement | null = null;
let root: Root | null = null;

async function mountEditor(
  value: string,
  onChange: (text: string) => void,
  editorRef: RefObject<MentionEditorHandle | null>,
  props: Partial<ComponentProps<typeof MentionEditor>> = {},
) {
  root = createRoot(container!);
  await act(async () => {
    root!.render(
      <MentionEditor
        {...props}
        ref={editorRef}
        value={value}
        onChange={onChange}
      />,
    );
  });
  return editorRef.current!.element!;
}

function dispatchKey(editor: HTMLElement, key: string) {
  const event = new KeyboardEvent('keydown', {
    key,
    bubbles: true,
    cancelable: true,
  });
  act(() => editor.dispatchEvent(event));
  return event;
}

function selectAll(editor: HTMLElement) {
  const range = document.createRange();
  range.selectNodeContents(editor);
  const selection = window.getSelection()!;
  selection.removeAllRanges();
  selection.addRange(range);
}

describe('MentionEditor 轻量交互', () => {
  beforeEach(() => {
    (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
    container = document.createElement('div');
    document.body.appendChild(container);
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
      callback(0);
      return 1;
    });
    vi.stubGlobal('cancelAnimationFrame', () => undefined);
  });

  afterEach(async () => {
    if (root) {
      await act(async () => root!.unmount());
    }
    root = null;
    container?.remove();
    container = null;
    window.getSelection()?.removeAllRanges();
    vi.unstubAllGlobals();
    delete (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT;
  });

  it('组合输入期间不提交，组合结束后提交中文内容', async () => {
    const onChange = vi.fn();
    const editorRef = createRef<MentionEditorHandle>();
    const editor = await mountEditor('', onChange, editorRef);

    act(() => {
      editor.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
      editor.replaceChildren(document.createTextNode('中文'));
      editor.dispatchEvent(new InputEvent('input', {
        bubbles: true,
        inputType: 'insertCompositionText',
        data: '中文',
      }));
    });
    expect(onChange).not.toHaveBeenCalled();

    act(() => {
      editor.dispatchEvent(new CompositionEvent('compositionend', {
        bubbles: true,
        data: '中文',
      }));
    });
    expect(onChange).toHaveBeenCalledTimes(1);
    expect(onChange).toHaveBeenLastCalledWith('中文');
  });

  it('方向键跨过连续提及，鼠标落在标签与空格之间时吸附到标签后', async () => {
    const editorRef = createRef<MentionEditorHandle>();
    const onChange = vi.fn();
    const editor = await mountEditor('@dev @qa ', onChange, editorRef);

    act(() => editorRef.current!.setSelection(0));
    dispatchKey(editor, 'ArrowRight');
    expect(editorRef.current!.getSelection()).toEqual({ start: 5, end: 5 });
    dispatchKey(editor, 'ArrowRight');
    expect(editorRef.current!.getSelection()).toEqual({ start: 9, end: 9 });
    dispatchKey(editor, 'ArrowLeft');
    expect(editorRef.current!.getSelection()).toEqual({ start: 4, end: 4 });
    dispatchKey(editor, 'ArrowLeft');
    expect(editorRef.current!.getSelection()).toEqual({ start: 0, end: 0 });

    act(() => editorRef.current!.setSelection(4));
    act(() => editor.dispatchEvent(new MouseEvent('mouseup', { bubbles: true })));
    expect(editorRef.current!.getSelection()).toEqual({ start: 5, end: 5 });

    const originalRect = Object.getOwnPropertyDescriptor(Range.prototype, 'getBoundingClientRect');
    Object.defineProperty(Range.prototype, 'getBoundingClientRect', {
      configurable: true,
      value: () => new DOMRect(68, 10, 4, 20),
    });
    try {
      // 浏览器可能把共享空格上的点击错误落到整行末尾；仍应按真实点击点吸附。
      act(() => editorRef.current!.setSelection(9));
      act(() => editor.dispatchEvent(new MouseEvent('mouseup', {
        bubbles: true,
        clientX: 70,
        clientY: 20,
      })));
      expect(editorRef.current!.getSelection()).toEqual({ start: 5, end: 5 });
    } finally {
      if (originalRect) {
        Object.defineProperty(Range.prototype, 'getBoundingClientRect', originalRect);
      } else {
        delete (Range.prototype as Range & { getBoundingClientRect?: () => DOMRect })
          .getBoundingClientRect;
      }
    }

    act(() => editorRef.current!.setSelection(4));
    dispatchKey(editor, 'x');
    expect(onChange).toHaveBeenLastCalledWith('@dev x @qa ');
  });

  it('跨标签全选删除后提交空内容，不留下浏览器占位换行', async () => {
    const onChange = vi.fn();
    const editorRef = createRef<MentionEditorHandle>();
    const editor = await mountEditor('@dev\n第二行 @qa ', onChange, editorRef);

    act(() => selectAll(editor));
    const event = dispatchKey(editor, 'Delete');

    expect(event.defaultPrevented).toBe(true);
    expect(onChange).toHaveBeenLastCalledWith('');

    act(() => {
      editor.replaceChildren(document.createElement('br'));
      editor.dispatchEvent(new InputEvent('input', { bubbles: true }));
    });
    expect(onChange).toHaveBeenLastCalledWith('');
  });

  it('多行纯文本粘贴保留换行并清除不同平台的换行差异', async () => {
    const onChange = vi.fn();
    const editorRef = createRef<MentionEditorHandle>();
    const editor = await mountEditor('开头', onChange, editorRef);
    act(() => editorRef.current!.focus());

    const pasteEvent = new Event('paste', { bubbles: true, cancelable: true });
    Object.defineProperty(pasteEvent, 'clipboardData', {
      value: {
        getData: (type: string) => type === 'text/plain' ? '第一行\r\n第二行\r第三行' : '',
      },
    });
    act(() => editor.dispatchEvent(pasteEvent));

    expect(pasteEvent.defaultPrevented).toBe(true);
    expect(onChange).toHaveBeenLastCalledWith('开头第一行\n第二行\n第三行');
  });

  it('历史消息编辑状态复用提及编辑器并整块删除标签', async () => {
    const editorRef = createRef<MentionEditorHandle>();
    const onSetEditingContent = vi.fn();
    const group: MessageGroup = {
      key: 'message-1',
      type: 'user',
      messages: [{
        id: 'message-1',
        role: 'user',
        content: [{ type: 'text', text: '请 @dev 处理' }],
        reasoning_content: '',
        created_at: '2026-07-28 12:00:00',
      }],
    };

    root = createRoot(container!);
    await act(async () => {
      root!.render(
        <UserMessageGroup
          group={group}
          runStatus="idle"
          nonEditableIds={new Set()}
          voiceMessages={{}}
          editingMessageId="message-1"
          editingContent="请 @dev 处理"
          editingAttachments={[]}
          editingTextareaRef={editorRef}
          onStartEdit={vi.fn()}
          onConfirmEdit={vi.fn()}
          onCancelEdit={vi.fn()}
          onSetEditingContent={onSetEditingContent}
          onSetEditingAttachments={vi.fn()}
          onAttachFiles={vi.fn()}
          onEditPaste={vi.fn()}
        />,
      );
    });

    const editor = editorRef.current!.element!;
    expect(editor.querySelector('[data-mention-token="@dev"]')).not.toBeNull();
    act(() => editorRef.current!.setSelection(7));
    dispatchKey(editor, 'Backspace');
    expect(onSetEditingContent).toHaveBeenLastCalledWith('请 处理');
  });
});
