import { useCallback } from 'react';
import type React from 'react';

import { shouldPreventArrowKey } from '@/lib/arrowKeyBoundaryGuard';

/**
 * 合并用户传入的 onKeyDown 与组件内置的"方向键边界拦截"。
 *
 * WKWebView 在输入框光标处于边界时，会把方向键转义序列误当文本输入，
 * 渲染成方格字符。此 hook 在 keydown 阶段对边界方向键调用 preventDefault 兜底。
 *
 * 行为约定：
 * - 先执行用户传入的 onKeyDown；
 * - 若用户已 preventDefault，则尊重，不重复拦截；
 * - 否则在判定为边界方向键时拦截。
 *
 * @see shouldPreventArrowKey
 */
export function useArrowKeyBoundaryGuard<T extends HTMLInputElement | HTMLTextAreaElement>(
  userOnKeyDown?: React.KeyboardEventHandler<T>,
) {
  return useCallback<React.KeyboardEventHandler<T>>(
    (e) => {
      userOnKeyDown?.(e);
      if (e.defaultPrevented) return;
      if (shouldPreventArrowKey(e.currentTarget, e.key)) {
        e.preventDefault();
      }
    },
    [userOnKeyDown],
  );
}
