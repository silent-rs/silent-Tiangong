/**
 * 插件 UI 容器共享的宿主上下文：主题 token 采集。
 *
 * iframe 容器经 postMessage 推送 `tiangong_host_context`；
 * Shadow 容器把同名 token 写入 shadow root 的 `:host` CSS 变量。
 * 两种容器 token 同源，插件 UI 用同一套变量名消费。
 */

export type HostTheme = 'light' | 'dark';

/** 宿主暴露给插件的设计 token 名（同时是 CSS 变量名）。 */
export const HOST_TOKEN_NAMES = [
  'background',
  'foreground',
  'card',
  'card-foreground',
  'muted',
  'muted-foreground',
  'accent',
  'accent-foreground',
  'primary',
  'primary-foreground',
  'destructive',
  'border',
  'input',
  'ring',
  'status-success',
  'status-warning',
  'status-error',
  'status-info',
  'radius',
] as const;

/** 从主文档读取当前主题 token 值。 */
export function collectHostTokens(): Record<string, string> {
  const styles = getComputedStyle(document.documentElement);
  return Object.fromEntries(
    HOST_TOKEN_NAMES.map((name) => [name, styles.getPropertyValue(`--${name}`).trim()]),
  );
}

/** iframe 容器的 hostContext 消息体（沿用既有协议，保持 v1 插件兼容）。 */
export function hostContext(theme: HostTheme, channel: string) {
  return {
    type: 'tiangong_host_context',
    channel,
    theme,
    tokens: collectHostTokens(),
    fontFamily: getComputedStyle(document.body).fontFamily,
  };
}

/** 把主题 token 写入 shadow root 的 `:host` CSS 变量。 */
export function applyShadowThemeTokens(shadow: ShadowRoot, theme: HostTheme) {
  const tokens = collectHostTokens();
  const declarations = Object.entries(tokens)
    .filter(([, value]) => value !== '')
    .map(([name, value]) => `--${name}: ${value};`)
    .join('\n');
  let style = shadow.querySelector<HTMLStyleElement>('style[data-host-tokens]');
  if (!style) {
    style = document.createElement('style');
    style.setAttribute('data-host-tokens', '');
    shadow.prepend(style);
  }
  style.textContent = `:host {\n${declarations}\n--host-theme: ${theme};\n}`;
}
