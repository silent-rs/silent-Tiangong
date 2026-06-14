import { useRef, useEffect, useCallback, useState } from 'react';
import { api } from '@/api/tauri';
import { useStore } from '@/store/useStore';
import { listen } from '@tauri-apps/api/event';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { RotateCw } from 'lucide-react';
import { Button } from './ui/button';

import '@xterm/xterm/css/xterm.css';

interface TerminalEntry {
  term: Terminal;
  fitAddon: FitAddon;
  lastAccessed: number;
  container: HTMLDivElement;
}

const MAX_POOL_SIZE = 5;

const TERMINAL_CONFIG = {
  cursorBlink: true,
  fontSize: 13,
  fontFamily: 'Menlo, Monaco, "Courier New", monospace',
  theme: {
    background: '#1e1e2e',
    foreground: '#cdd6f4',
    cursor: '#f5e0dc',
    selectionBackground: '#585b7066',
  },
  convertEol: false,
  scrollback: 10000,
};

/**
 * 终端面板（与旧分支 UX 对齐）：
 * - 每个 session 一个独立子容器 div，切换时 display 切换不重建 xterm
 * - 前端池化最多 5 个 xterm，淘汰老的但保留后端 PTY（下次切回时从历史输出恢复）
 * - 显式调用 ensureSession 确认 PTY 启动成功
 * - 工具栏：cwd 显示 + 重置按钮（重启 shell）
 */
export function TerminalPanel({ onClose }: { onClose: () => void }) {
  const sessionCwd = useStore((s) => s.sessionCwd);
  const workspaceDir = useStore((s) => s.workspaceDir);

  const containerRef = useRef<HTMLDivElement>(null);
  const poolRef = useRef<Map<string, TerminalEntry>>(new Map());
  const currentIdRef = useRef<string>('');
  const globalUnlistenRef = useRef<(() => void) | null>(null);
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const pendingCreateRef = useRef<Set<string>>(new Set());
  const createdTimeRef = useRef<number>(0);

  const [displayInfo, setDisplayInfo] = useState<{
    cwd: string;
    alive: boolean;
    error?: string;
  }>({
    cwd: '',
    alive: false,
  });

  // 单 PTY 模型：effectiveId 固定为系统 PTY 的 session_id（由后端分配的 SCRU128）。
  // agent 命令和面板操作共享同一条终端会话，历史互通。
  const [systemSessionId, setSystemSessionId] = useState<string | null>(null);
  useEffect(() => {
    let cancelled = false;
    api.terminalSystemSessionInfo()
      .then((info) => {
        if (!cancelled) setSystemSessionId(info.session_id);
      })
      .catch(() => {
        if (!cancelled) setSystemSessionId('__tiangong_system__');
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const effectiveId = systemSessionId;

  // 全局 output listener（按 session_id 分发）
  useEffect(() => {
    let cancelled = false;
    const setup = async () => {
      const unlisten = await listen<{ session_id: string; text: string }>(
        'terminal:output',
        (event) => {
          if (cancelled) return;
          const { session_id, text } = event.payload;
          if (!text) return;
          const entry = poolRef.current.get(session_id);
          if (entry) {
            entry.term.write(text);
          }
        },
      );
      if (cancelled) {
        unlisten();
        return;
      }
      globalUnlistenRef.current = unlisten;
    };
    setup();
    return () => {
      cancelled = true;
      globalUnlistenRef.current?.();
      globalUnlistenRef.current = null;
    };
  }, []);

  // 容器尺寸变化时 fit
  useEffect(() => {
    if (!containerRef.current) return;
    const observer = new ResizeObserver(() => {
      const entry = poolRef.current.get(currentIdRef.current);
      if (entry) {
        try {
          entry.fitAddon.fit();
        } catch {
          // ignore
        }
      }
    });
    observer.observe(containerRef.current);
    resizeObserverRef.current = observer;
    return () => {
      observer.disconnect();
      resizeObserverRef.current = null;
    };
  }, []);

  // 切换对话时挂载/创建 xterm（仅在 effectiveId 变化时触发）
  useEffect(() => {
    // 系统 PTY id 尚未就绪时跳过（单 PTY 模型下 effectiveId 即系统 id）
    if (!effectiveId) return;
    if (effectiveId === currentIdRef.current && poolRef.current.has(effectiveId)) {
      return;
    }
    const container = containerRef.current;
    if (!container) return;

    // 单 PTY 模型：effectiveId 固定为系统 PTY，不再有「草稿终端」与销毁逻辑。
    currentIdRef.current = effectiveId;

    const switchTo = async () => {
      if (pendingCreateRef.current.has(effectiveId)) return;
      pendingCreateRef.current.add(effectiveId);

      try {
        let entry = poolRef.current.get(effectiveId);

        if (!entry) {
          const cwd = sessionCwd || workspaceDir || '';

          let alive: boolean;
          try {
            alive = await api.terminalEnsureSession(effectiveId, cwd);
          } catch (e) {
            setDisplayInfo({ cwd: cwd || '终端', alive: false, error: String(e) });
            return;
          }
          if (!alive) {
            setDisplayInfo({ cwd: cwd || '终端', alive: false, error: 'PTY 启动失败' });
            return;
          }

          const term = new Terminal(TERMINAL_CONFIG);
          const fitAddon = new FitAddon();
          term.loadAddon(fitAddon);

          term.onData((data) => {
            const sid = currentIdRef.current;
            if (sid) api.terminalSessionSendInput(sid, data).catch(() => {});
          });

          term.onResize(({ cols, rows }) => {
            const sid = currentIdRef.current;
            if (sid) api.terminalSessionResize(sid, cols, rows).catch(() => {});
          });

          // 独立子容器
          const subContainer = document.createElement('div');
          subContainer.style.cssText = 'width:100%;height:100%;display:none;';
          const parentContainer = containerRef.current;
          if (!parentContainer) return;
          parentContainer.appendChild(subContainer);
          term.open(subContainer);

          // 加载历史输出（\n 转 \r\n 让 xterm 正确换行）
          try {
            const history = await api.terminalSessionRecentOutput(effectiveId, 5000);
            if (history) {
              term.write(history.replace(/\n/g, '\r\n'));
            }
          } catch {
            // ignore
          }

          entry = { term, fitAddon, lastAccessed: Date.now(), container: subContainer };
          poolRef.current.set(effectiveId, entry);
          createdTimeRef.current = Date.now();

          // 淘汰最久未访问的前端 xterm 实例（保留后端 PTY）
          while (poolRef.current.size > MAX_POOL_SIZE) {
            let oldest: string | null = null;
            let oldestTime = Infinity;
            for (const [id, e] of poolRef.current) {
              if (id !== effectiveId && e.lastAccessed < oldestTime) {
                oldest = id;
                oldestTime = e.lastAccessed;
              }
            }
            if (oldest) {
              const old = poolRef.current.get(oldest);
              if (old) {
                old.container.remove();
                old.term.dispose();
              }
              poolRef.current.delete(oldest);
            } else {
              break;
            }
          }
        }

        if (entry) {
          entry.lastAccessed = Date.now();
        }

        // 切换显示
        const parentContainer = containerRef.current;
        if (!parentContainer) return;
        for (const [, e] of poolRef.current) {
          e.container.style.display = 'none';
        }
        entry!.container.style.display = 'block';
        requestAnimationFrame(() => {
          try {
            entry!.fitAddon.fit();
          } catch {
            // ignore
          }
        });

        // 更新工具栏
        try {
          const info = await api.terminalSessionStatus(effectiveId);
          setDisplayInfo({ cwd: info.cwd || sessionCwd || workspaceDir || '终端', alive: info.alive });
        } catch {
          setDisplayInfo({ cwd: sessionCwd || workspaceDir || '终端', alive: true });
        }
      } finally {
        pendingCreateRef.current.delete(effectiveId);
      }
    };
    switchTo();
  }, [effectiveId, sessionCwd, workspaceDir]);

  // CWD 变化时单独同步（不触发终端重建）
  useEffect(() => {
    if (!currentIdRef.current) return;
    if (Date.now() - createdTimeRef.current < 3000) return;
    const targetCwd = sessionCwd || workspaceDir;
    if (targetCwd) {
      api.terminalSessionSetCwd(currentIdRef.current, targetCwd).catch(() => {});
    }
  }, [sessionCwd, workspaceDir]);

  // 卸载时只销毁前端 xterm（保留后端 PTY 和历史）
  useEffect(() => {
    return () => {
      for (const [, entry] of poolRef.current) {
        entry.container.remove();
        entry.term.dispose();
      }
      poolRef.current.clear();
    };
  }, []);

  const handleReset = useCallback(async () => {
    const id = currentIdRef.current;
    if (!id) return;
    try {
      await api.terminalSessionReset(id);
      const entry = poolRef.current.get(id);
      entry?.term.clear();
      setDisplayInfo((prev) => ({ ...prev, alive: true }));
    } catch {
      // ignore
    }
  }, []);

  const displayCwd = sessionCwd || workspaceDir || displayInfo.cwd || '终端';

  return (
    <div className="flex flex-col h-full bg-[#1e1e2e]">
      {/* 工具栏 */}
      <div className="flex items-center justify-between px-3 py-1 border-b border-border shrink-0 bg-background">
        <div className="flex items-center gap-2 min-w-0">
          <span className="text-xs text-muted-foreground font-mono truncate">
            {displayInfo.alive ? displayCwd : displayInfo.error || '终端未就绪'}
          </span>
        </div>
        <div className="flex items-center gap-1 shrink-0">
          <Button
            variant="ghost"
            size="icon"
            className="h-5 w-5 text-muted-foreground hover:text-foreground"
            onClick={handleReset}
            title="重置终端"
          >
            <RotateCw className="w-3 h-3" />
          </Button>
          <Button
            variant="ghost"
            size="icon"
            className="h-5 w-5 text-muted-foreground hover:text-foreground"
            onClick={onClose}
            title="关闭面板"
          >
            <span className="text-sm leading-none">×</span>
          </Button>
        </div>
      </div>

      {/* xterm.js 容器 */}
      <div ref={containerRef} className="flex-1 min-h-0" />
    </div>
  );
}
