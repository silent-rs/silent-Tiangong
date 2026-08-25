// devkit 公共约定：路径、校验与输出辅助。
import { existsSync } from 'node:fs';
import { join } from 'node:path';
import { homedir } from 'node:os';

export function devRoot(ctx) {
  return ctx?.rootOverride ?? process.env.TIANGONG_PLUGINS_DEV ?? join(homedir(), '.tiangong', 'plugins-dev');
}

export function storageRoot() {
  return join(homedir(), '.tiangong');
}

export function validId(id) {
  return /^[A-Za-z0-9._-]+$/.test(id) && id !== '.' && id !== '..';
}

export function requireProject(ctx, id) {
  if (!validId(id)) {
    throw Object.assign(new Error(`插件 ID 只能包含字母数字与 - _ .：${id}`), { code: 'BAD_ID' });
  }
  const projectDir = join(devRoot(ctx), id);
  if (!existsSync(join(projectDir, '.plugin-dev.json'))) {
    throw Object.assign(
      new Error(`项目 ${id} 不存在（${projectDir}）。先执行 init，或用 --root 指定开发根。`),
      { code: 'NO_PROJECT' },
    );
  }
  return projectDir;
}

export function fail(message) {
  return { ok: false, error: message };
}
