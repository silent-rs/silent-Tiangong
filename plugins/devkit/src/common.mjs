// devkit 公共约定：路径、校验、输出与子进程辅助。
import { existsSync, lstatSync } from 'node:fs';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';
import { homedir } from 'node:os';
import { spawn } from 'node:child_process';

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

/**
 * 解析 manifest 声明的相对路径（ui.entry / resources[]），确保落在 root 内。
 * 拒绝：空路径、绝对路径、`..` 分量、Windows drive/UNC 前缀。
 * build 与 validate 共用，防读项目外文件 / 写 release 外目录。
 */
export function resolveInside(root, relativePath, label) {
  if (typeof relativePath !== 'string' || relativePath.trim() === '') {
    throw Object.assign(new Error(`${label} 不能为空`), { code: 'PATH_ESCAP' });
  }
  if (isAbsolute(relativePath) || /^[a-zA-Z]:[\\/]/.test(relativePath) || relativePath.startsWith('\\\\')) {
    throw Object.assign(new Error(`${label} 必须是相对路径：${relativePath}`), { code: 'PATH_ESCAP' });
  }
  if (relativePath.split(/[\\/]+/).includes('..')) {
    throw Object.assign(new Error(`${label} 含路径逃逸（..）：${relativePath}`), { code: 'PATH_ESCAP' });
  }
  const resolvedRoot = resolve(root);
  const resolved = resolve(root, relativePath);
  if (resolved !== resolvedRoot && !resolved.startsWith(`${resolvedRoot}${sep}`)) {
    throw Object.assign(new Error(`${label} 路径越界：${relativePath}`), { code: 'PATH_ESCAP' });
  }
  return resolved;
}

/**
 * 逐级断言 root → target 的每个路径分量都不是符号链接。
 * path.resolve 是纯字符串运算、不解析符号链接，resolveInside 无法发现
 * 链接指向项目外的实体；必须在文件系统层面逐级 lstat（任何一级为
 * 符号链接即拒绝，含中间目录与目标自身）。
 */
export function assertNoSymlinkPath(root, target) {
  const resolvedRoot = resolve(root);
  const parts = relative(resolvedRoot, resolve(target)).split(/[\\/]+/).filter((part) => part && part !== '.');
  let current = resolvedRoot;
  for (const part of parts) {
    current = join(current, part);
    let stat;
    try {
      stat = lstatSync(current);
    } catch {
      // 后续分量不存在：由调用方的存在性检查负责，此处不再深入。
      return;
    }
    if (stat.isSymbolicLink()) {
      throw Object.assign(new Error(`路径含符号链接，拒绝读取：${current}`), { code: 'SYMLINK' });
    }
  }
}

/** 目标存在且为目录内普通实体（拒绝符号链接）。 */
export function isRealDirectory(path) {
  if (!existsSync(path)) return false;
  const stat = lstatSync(path);
  return stat.isDirectory() && !stat.isSymbolicLink();
}

/**
 * 终止整个进程树：Unix 经独立进程组（spawn detached）发组信号；
 * Windows 用 taskkill /T /F。等待进程真正退出后回调。
 */
export function killTree(child) {
  if (process.platform === 'win32') {
    spawn('taskkill', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore' });
  } else if (child.pid) {
    try {
      process.kill(-child.pid, 'SIGKILL');
    } catch {
      // 进程组已不存在：单独杀子进程兜底。
      try {
        child.kill('SIGKILL');
      } catch {
        // 已退出。
      }
    }
  }
}

/** 子进程公共 spawn 选项：Unix 独立进程组（killTree 依赖）。 */
export function spawnOptions(extra) {
  return {
    stdio: ['ignore', 'pipe', 'pipe'],
    ...extra,
    ...(process.platform !== 'win32' ? { detached: true } : {}),
  };
}
