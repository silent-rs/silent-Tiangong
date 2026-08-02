import { useState, useEffect, type RefObject } from 'react';

const PLUGIN_MASK_MESSAGE = 'plugin_host_mask';
const DEFAULT_MASK_COLOR = 'rgba(0, 0, 0, 0.5)';
const MAX_COLOR_LENGTH = 100;

interface PluginMaskState {
  channel: string;
  color: string;
}

function resolveMaskColor(value: unknown): string {
  if (typeof value !== 'string') return DEFAULT_MASK_COLOR;
  const color = value.trim();
  if (
    !color
    || color.length > MAX_COLOR_LENGTH
    || typeof CSS === 'undefined'
    || !CSS.supports('color', color)
  ) {
    return DEFAULT_MASK_COLOR;
  }
  return color;
}

export function usePluginMask(
  iframeRef: RefObject<HTMLIFrameElement | null>,
  channel: string,
): string | null {
  const [mask, setMask] = useState<PluginMaskState | null>(null);

  useEffect(() => {
    const source = iframeRef.current?.contentWindow;
    if (!source) return;

    const handler = (event: MessageEvent) => {
      if (event.source !== source) return;
      const data = event.data;
      if (
        !data
        || data.type !== PLUGIN_MASK_MESSAGE
        || data.channel !== channel
        || typeof data.visible !== 'boolean'
      ) {
        return;
      }

      setMask(data.visible
        ? { channel, color: resolveMaskColor(data.color) }
        : null);
    };

    window.addEventListener('message', handler);
    return () => window.removeEventListener('message', handler);
  }, [channel, iframeRef]);

  return mask?.channel === channel ? mask.color : null;
}
