// logs：读取构建/试运行/已安装插件日志尾部。
import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { devRoot, fail, storageRoot, validId } from './common.mjs';

const MAX_TAIL_BYTES = 5 * 1024 * 1024;

export async function logs(argv, ctx) {
  const [target] = argv.filter((arg) => !arg.startsWith('--'));
  const linesIndex = argv.indexOf('--lines');
  const lines = Math.min(Math.max(linesIndex !== -1 ? Number(argv[linesIndex + 1]) || 100 : 100, 1), 1000);
  if (!target || !target.includes(':')) {
    return fail('用法：logs <dev:项目id|plugin:插件id> [--lines N]');
  }
  const [kind, id] = target.split(':');
  if (!validId(id)) return fail(`目标 ID 非法：${id}`);
  let logDir;
  if (kind === 'dev') {
    logDir = join(devRoot(ctx), id, 'logs');
  } else if (kind === 'plugin') {
    logDir = join(storageRoot(), 'plugins', id, 'logs');
  } else {
    return fail('日志目标必须是 dev:<项目id> 或 plugin:<插件id>');
  }
  const newest = newestLog(logDir);
  if (!newest) return { ok: false, error: `暂无日志（${logDir} 不存在或为空）` };
  const content = tail(newest);
  const tailLines = content.split('\n').filter((line) => line !== '').slice(-lines);
  return { ok: true, path: newest, lines: tailLines };
}

function newestLog(logDir) {
  if (!existsSync(logDir)) return null;
  let best = null;
  let bestMtime = 0;
  for (const name of readdirSync(logDir)) {
    if (!name.endsWith('.log')) continue;
    const path = join(logDir, name);
    const mtime = statSync(path).mtimeMs;
    if (mtime > bestMtime) {
      best = path;
      bestMtime = mtime;
    }
  }
  return best;
}

function tail(path) {
  const text = readFileSync(path, 'utf8');
  if (text.length <= MAX_TAIL_BYTES) return text;
  return text.slice(text.length - MAX_TAIL_BYTES);
}
