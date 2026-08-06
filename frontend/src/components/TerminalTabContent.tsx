import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { api } from '@/api/tauri';
import { useStore } from '@/store/useStore';
import { listen } from '@tauri-apps/api/event';
import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import { RotateCw } from 'lucide-react';
import { Button } from './ui/button';

import '@xterm/xterm/css/xterm.css';

interface TerminalTabContentProps {
  sessionId: string;
  tabId: string;
  isActive: boolean;
}

const SCREEN_SNAPSHOT_THROTTLE_MS = 50;

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

function snapshotVisibleScreen(term: Terminal): string {
  const buffer = term.buffer.active;
  const start = buffer.baseY;
  const end = start + term.rows;
  const lines: string[] = [];
  for (let y = start; y < end; y++) {
    const line = buffer.getLine(y);
    lines.push(line ? line.translateToString(true) : '');
  }
  while (lines.length > 0 && lines[lines.length - 1].trim() === '') {
    lines.pop();
  }
  return lines.join('\n');
}

export function TerminalTabContent({ sessionId, tabId, isActive }: TerminalTabContentProps) {
  const sessionCwd = useStore((s) => s.sessionCwd);
  const workspaceDir = useStore((s) => s.workspaceDir);

  const terminalId = useMemo(() => `${sessionId}:${tabId}`, [sessionId, tabId]);
  const containerRef = useRef<HTMLDivElement>(null);
  const terminalRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const inputBufferRef = useRef('');
  const lastSnapshotPushRef = useRef(0);
  const creatingRef = useRef(false);

  const [displayInfo, setDisplayInfo] = useState<{
    cwd: string;
    alive: boolean;
    error?: string;
  }>({
    cwd: '',
    alive: false,
  });
  const [ptyVersion, setPtyVersion] = useState(0);

  const pushScreenSnapshot = useCallback(() => {
    const term = terminalRef.current;
    if (!term) return;
    const now = Date.now();
    if (now - lastSnapshotPushRef.current < SCREEN_SNAPSHOT_THROTTLE_MS) {
      return;
    }
    lastSnapshotPushRef.current = now;
    api.terminalSessionUpdateScreen(terminalId, snapshotVisibleScreen(term)).catch(() => {});
  }, [terminalId]);

  useEffect(() => {
    let cancelled = false;
    const unlisteners: Array<() => void | Promise<void>> = [];
    const release = (unlisten: () => void | Promise<void>) => {
      try {
        void Promise.resolve(unlisten()).catch(() => {});
      } catch {
        // WebView reload or a concurrent cleanup may have already removed it.
      }
    };
    const setup = async () => {
      const unlistenOutput = await listen<{ session_id: string; text: string }>(
        'terminal:output',
        (event) => {
          if (cancelled) return;
          const { session_id, text } = event.payload;
          if (session_id !== terminalId || !text) return;
          terminalRef.current?.write(text);
          pushScreenSnapshot();
        },
      );
      if (cancelled) {
        release(unlistenOutput);
        return;
      }
      unlisteners.push(unlistenOutput);
      const unlistenReset = await listen('terminal:reset', () => {
        if (cancelled) return;
        terminalRef.current?.dispose();
        terminalRef.current = null;
        fitAddonRef.current = null;
        setDisplayInfo({ cwd: '', alive: false });
        setPtyVersion((version) => version + 1);
      });
      if (cancelled) {
        release(unlistenReset);
        return;
      }
      unlisteners.push(unlistenReset);
    };

    setup().catch(() => {});
    return () => {
      cancelled = true;
      unlisteners.splice(0).forEach(release);
    };
  }, [pushScreenSnapshot, terminalId]);

  useEffect(() => {
    if (!containerRef.current) return;
    const observer = new ResizeObserver(() => {
      if (!isActive) return;
      try {
        fitAddonRef.current?.fit();
      } catch {
        // ignore
      }
    });
    observer.observe(containerRef.current);
    return () => observer.disconnect();
  }, [isActive]);

  useEffect(() => {
    if (!isActive || !containerRef.current || creatingRef.current) return;

    const mountTerminal = async () => {
      creatingRef.current = true;
      try {
        const cwd = sessionCwd || workspaceDir || '';
        await api.terminalTabRestore(sessionId, tabId, '终端', cwd);
        await api.terminalTabSwitch(sessionId, tabId);
        const alive = await api.terminalEnsureSession(terminalId, cwd);
        if (!alive) {
          setDisplayInfo({ cwd: cwd || '终端', alive: false, error: 'PTY 启动失败' });
          return;
        }

        if (!terminalRef.current) {
          const term = new Terminal(TERMINAL_CONFIG);
          const fitAddon = new FitAddon();
          term.loadAddon(fitAddon);
          term.onResize(({ cols, rows }) => {
            api.terminalSessionResize(terminalId, cols, rows).catch(() => {});
          });
          const container = containerRef.current;
          if (!container) return;
          term.open(container);

          const history = await api.terminalSessionRecentOutput(terminalId, 5000).catch(() => '');
          if (history) {
            await new Promise<void>((resolve) => {
              term.write(history.replace(/\n/g, '\r\n'), resolve);
            });
          }

          // 历史回放期间不连接 PTY 输入。历史日志可能来自旧版本并带有终端查询，
          // xterm 对查询生成的响应只能用于实时输出，不能写进当前 shell 输入行。
          term.onData((data) => {
            api
              .terminalSessionSendInput(terminalId, data)
              .then(() => {
                setDisplayInfo((current) =>
                  current.alive ? current : { ...current, alive: true, error: undefined },
                );
              })
              .catch((error) => {
                setDisplayInfo((current) => ({
                  ...current,
                  alive: false,
                  error: `终端输入失败：${String(error)}`,
                }));
              });

            if (data === '\r') {
              const command = inputBufferRef.current.trim();
              if (command) {
                api.terminalReportUserCommand(terminalId, command).catch(() => {});
              }
              inputBufferRef.current = '';
            } else if (data === '\x7f' || data === '\b') {
              inputBufferRef.current = inputBufferRef.current.slice(0, -1);
            } else if (data[0] === '\x1b') {
              // ignore escape sequences
            } else if (data.length === 1 && data >= ' ') {
              inputBufferRef.current += data;
            } else if (data.length > 1) {
              const lines = data.split(/\r\n|\r|\n/);
              lines.forEach((line, index) => {
                const clean = line.replace(/[\x00-\x1f\x7f]/g, '');
                inputBufferRef.current += clean;
                if (index < lines.length - 1) {
                  const command = inputBufferRef.current.trim();
                  if (command) {
                    api.terminalReportUserCommand(terminalId, command).catch(() => {});
                  }
                  inputBufferRef.current = '';
                }
              });
            }
          });
          terminalRef.current = term;
          fitAddonRef.current = fitAddon;
        }

        requestAnimationFrame(() => {
          try {
            fitAddonRef.current?.fit();
          } catch {
            // ignore
          }
          // xterm 的输入捕获依赖一个隐藏的辅助 textarea 获得焦点：
          // 没有焦点时 onData 不触发，键盘输入被丢弃（Windows WebView2 下
          // 尤其明显，容器不会从父元素继承焦点）。终端 open 后主动聚焦，
          // 同时覆盖 terminal:reset 重建（全新 textarea）的路径。
          try {
            terminalRef.current?.focus();
          } catch {
            // ignore
          }
        });

        const info = await api.terminalSessionStatus(terminalId).catch(() => null);
        setDisplayInfo({
          cwd: info?.cwd || sessionCwd || workspaceDir || '终端',
          alive: info?.alive ?? true,
        });
      } catch (error) {
        setDisplayInfo({ cwd: sessionCwd || workspaceDir || '终端', alive: false, error: String(error) });
      } finally {
        creatingRef.current = false;
      }
    };

    mountTerminal();
  }, [isActive, ptyVersion, sessionCwd, sessionId, tabId, terminalId, workspaceDir]);

  // Tab 激活时重新聚焦：终端已存在会跳过创建分支，焦点不会自动重建，
  // 需在此显式聚焦，否则切回终端 Tab 后键盘输入静默失效。
  useEffect(() => {
    if (!isActive) return;
    const raf = requestAnimationFrame(() => {
      try {
        terminalRef.current?.focus();
      } catch {
        // ignore
      }
    });
    return () => cancelAnimationFrame(raf);
  }, [isActive]);

  useEffect(() => {
    return () => {
      terminalRef.current?.dispose();
      terminalRef.current = null;
      fitAddonRef.current = null;
    };
  }, []);

  const handleReset = useCallback(async () => {
    try {
      await api.terminalSessionReset(terminalId);
      terminalRef.current?.clear();
      setDisplayInfo((current) => ({ ...current, alive: true, error: undefined }));
    } catch (error) {
      setDisplayInfo((current) => ({
        ...current,
        alive: false,
        error: `终端重置失败：${String(error)}`,
      }));
    }
  }, [terminalId]);

  const displayCwd = sessionCwd || workspaceDir || displayInfo.cwd || '终端';

  return (
    <div className={`h-full flex-col bg-[#1e1e2e] ${isActive ? 'flex' : 'hidden'}`}>
      <div className="flex shrink-0 items-center justify-between border-b border-border bg-background px-3 py-1">
        <span className="min-w-0 truncate font-mono text-xs text-muted-foreground">
          {displayInfo.alive ? displayCwd : displayInfo.error || '终端未就绪'}
        </span>
        <Button
          variant="ghost"
          size="icon"
          className="h-5 w-5 text-muted-foreground hover:text-foreground"
          onClick={handleReset}
          title="重置终端"
        >
          <RotateCw className="h-3 w-3" />
        </Button>
      </div>
      <div
        ref={containerRef}
        className="min-h-0 flex-1"
        onMouseDown={() => {
          try {
            terminalRef.current?.focus();
          } catch {
            // ignore
          }
        }}
      />
    </div>
  );
}
