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

// 草稿态终端的稳定临时 id（与 store 中 createSession 生成的一致）。
// 草稿态用此 id 创建 PTY；转正后前端把 pool 的 key 从此 id 迁移到真实 session_id。
const DRAFT_TERMINAL_ID = '__draft_terminal__';
// 屏幕快照回传节流间隔（毫秒）：xterm 每次输出后都序列化屏幕会过于频繁，
// 用此间隔合并短时间内的多次输出为一次回传。
const SCREEN_SNAPSHOT_THROTTLE_MS = 50;

/**
 * 序列化 xterm.js 当前可见屏幕（baseY..baseY+rows）为纯文本。
 * 这是终端真正渲染的画面（含 vim/nano 全屏界面），后端单行 processor 无法重建，
 * 由前端回传供 handle_exec_interactive 返回给 Agent。
 */
function snapshotVisibleScreen(term: Terminal): string {
  const buffer = term.buffer.active;
  const start = buffer.baseY;
  const end = start + term.rows;
  const lines: string[] = [];
  for (let y = start; y < end; y++) {
    const line = buffer.getLine(y);
    lines.push(line ? line.translateToString(true) : '');
  }
  // 裁掉尾部空行（vim 底部的 ~ 占位行保留，但纯空白行精简输出）
  while (lines.length > 0 && lines[lines.length - 1].trim() === '') {
    lines.pop();
  }
  return lines.join('\n');
}

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
 * 终端面板（按对话 PTY 模型）：
 * - effectiveId 取当前对话 id（activeSessionId），每个对话拥有独立 PTY
 * - 切换对话时懒创建/挂载对应对话的 xterm，历史输出与实时事件按 session_id 分发
 * - 池化结构（poolRef/MAX_POOL_SIZE）限制前端 xterm 实例数量，超出时淘汰最久未访问
 *   （仅销毁前端 xterm，后端 PTY 保留以便切回时恢复历史）
 * - 工具栏：cwd 显示 + 重置按钮（重启 shell 并清空日志）
 */
export function TerminalPanel({ onClose }: { onClose: () => void }) {
  const activeSessionId = useStore((s) => s.activeSessionId);
  const draftTerminalId = useStore((s) => s.draftTerminalId);
  const sessionCwd = useStore((s) => s.sessionCwd);
  const workspaceDir = useStore((s) => s.workspaceDir);

  const containerRef = useRef<HTMLDivElement>(null);
  const poolRef = useRef<Map<string, TerminalEntry>>(new Map());
  const currentIdRef = useRef<string>('');
  const globalUnlistenRef = useRef<Array<() => void>>([]);
  const resizeObserverRef = useRef<ResizeObserver | null>(null);
  const pendingCreateRef = useRef<Set<string>>(new Set());
  const createdTimeRef = useRef<number>(0);
  // 上次屏幕快照回传时间戳（节流用）
  const lastSnapshotPushRef = useRef<number>(0);
  // 用户输入行缓冲：按 session_id 独立累积，遇回车截断上报完整命令行。
  const inputBufferRef = useRef<Map<string, string>>(new Map());

  const [displayInfo, setDisplayInfo] = useState<{
    cwd: string;
    alive: boolean;
    error?: string;
  }>({
    cwd: '',
    alive: false,
  });
  // PTY 重建版本：terminal:reset 事件递增，作为 mount effect 依赖强制重新 ensure。
  // workspace 切换时后端销毁所有 PTY 后发出 reset，前端 pool 已清空，需重跑 ensure
  // 重建当前会话的 xterm（effectiveId 不变，仅靠它无法触发重挂载）。
  const [ptyVersion, setPtyVersion] = useState(0);

  // 按对话 PTY 模型：effectiveId 为当前对话 id。
  // 草稿态（activeSessionId=null）用 draftTerminalId 作为临时 id 创建草稿 PTY，
  // 转正后切换为真实 session_id（后端 PTY 已通过 terminalAttachSession 迁移归属）。
  const effectiveId = activeSessionId || draftTerminalId || null;

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
            // write 是同步的，写完 buffer 立即可读。节流后把当前可见屏幕回传后端，
            // 供 handle_exec_interactive 检测变化并返回给 Agent（vi/nano 全屏界面）。
            const now = Date.now();
            if (now - lastSnapshotPushRef.current >= SCREEN_SNAPSHOT_THROTTLE_MS) {
              lastSnapshotPushRef.current = now;
              const snapshot = snapshotVisibleScreen(entry.term);
              api
                .terminalSessionUpdateScreen(session_id, snapshot)
                .catch(() => {});
            }
          }
        },
      );
      if (cancelled) {
        unlisten();
        return;
      }
      globalUnlistenRef.current.push(unlisten);
    };
    setup();
    return () => {
      cancelled = true;
      for (const fn of globalUnlistenRef.current) fn();
      globalUnlistenRef.current = [];
    };
  }, []);

  // 监听后端 terminal:reset 事件（workspace 切换等场景，后端销毁所有 PTY 后发出）。
  // 前端需丢弃所有 xterm 缓存，使切换对话的 ensure effect 重新用新 workspace 创建 PTY。
  useEffect(() => {
    let cancelled = false;
    const setup = async () => {
      const unlisten = await listen('terminal:reset', () => {
        if (cancelled) return;
        // 销毁所有缓存的 xterm entry（DOM 容器由 React 卸载/重挂）
        for (const entry of poolRef.current.values()) {
          try {
            entry.term.dispose();
          } catch {
            // ignore
          }
        }
        poolRef.current.clear();
        // 清空 currentIdRef，强制下方「切换对话挂载」effect 重建当前会话的 xterm
        currentIdRef.current = '';
        // 递增版本号触发 mount effect 重跑（effectiveId 不变时唯一可靠的重建入口）
        setPtyVersion((v) => v + 1);
        setDisplayInfo({ cwd: '', alive: false });
      });
      if (cancelled) {
        unlisten();
        return;
      }
      globalUnlistenRef.current.push(unlisten);
    };
    setup();
    return () => {
      cancelled = true;
      for (const fn of globalUnlistenRef.current) fn();
      globalUnlistenRef.current = [];
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
    // 按对话 PTY 模型：effectiveId 即当前对话 id，尚未就绪时跳过
    if (!effectiveId) return;

    // 草稿态转正过渡：effectiveId 从 DRAFT_TERMINAL_ID 切换到真实 session_id 时，
    // 把前端 pool 里的 xterm entry key 从草稿 id 迁移到真实 id。
    // 后端 PTY 已通过 terminalAttachSession 改名，前端 pool 同步改名后，
    // 输出 listener（按 session_id 分发）和后续 ensure（命中后端已迁移的 PTY）
    // 都能用真实 id 正确工作，避免重建 xterm 丢失草稿期已渲染的内容。
    if (
      effectiveId !== DRAFT_TERMINAL_ID &&
      effectiveId !== currentIdRef.current &&
      poolRef.current.has(DRAFT_TERMINAL_ID)
    ) {
      const draftEntry = poolRef.current.get(DRAFT_TERMINAL_ID);
      if (draftEntry) {
        poolRef.current.delete(DRAFT_TERMINAL_ID);
        poolRef.current.set(effectiveId, draftEntry);
      }
    }

    if (effectiveId === currentIdRef.current && poolRef.current.has(effectiveId)) {
      return;
    }
    const container = containerRef.current;
    if (!container) return;

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

            // 累积当前行，遇回车截断并上报完整命令（供注入 Agent 对话链）。
            if (!sid) return;
            if (data === '\r') {
              const cmd = (inputBufferRef.current.get(sid) ?? '').trim();
              if (cmd) {
                api.terminalReportUserCommand(sid, cmd).catch(() => {});
              }
              inputBufferRef.current.set(sid, '');
            } else if (data === '\x7f' || data === '\b') {
              const buf = inputBufferRef.current.get(sid) ?? '';
              inputBufferRef.current.set(sid, buf.slice(0, -1));
            } else if (data[0] === '\x1b') {
              // ESC 开头的序列（方向键/功能键/终端报告）：整体忽略
            } else if (data.length === 1 && data >= ' ') {
              const buf = inputBufferRef.current.get(sid) ?? '';
              inputBufferRef.current.set(sid, buf + data);
            } else if (data.length > 1) {
              // 粘贴多字符：按行分割处理，遇换行触发截断上报（与单字符回车一致）
              const lines = data.split(/\r\n|\r|\n/);
              for (let i = 0; i < lines.length; i++) {
                const line = lines[i].replace(/[\x00-\x1f\x7f]/g, '');
                if (line) {
                  const buf = inputBufferRef.current.get(sid) ?? '';
                  inputBufferRef.current.set(sid, buf + line);
                }
                // 非最后一行（含换行）→ 触发截断上报
                if (i < lines.length - 1) {
                  const cmd = (inputBufferRef.current.get(sid) ?? '').trim();
                  if (cmd) {
                    api.terminalReportUserCommand(sid, cmd).catch(() => {});
                  }
                  inputBufferRef.current.set(sid, '');
                }
              }
            }
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
  }, [effectiveId, ptyVersion]);

  // 会话独立终端模型：PTY 创建时已用 workspace cwd，之后 cwd 由用户/命令
  // 自然管理。切换会话时不强制 cd，避免覆盖用户在终端内的 cd 操作。

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
