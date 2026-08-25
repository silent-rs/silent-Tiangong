// run：开发期试运行——按项目 run.json 声明执行 npx -y <pkg> <script> [args]。
import { appendFileSync, lstatSync, mkdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { assertNoSymlinkPath, fail, killTree, requireProject, resolveInside, spawnOptions } from './common.mjs';

const RUN_TIMEOUT_MS = 120_000;
const OUTPUT_TAIL = 16 * 1024;

export async function run(argv, ctx) {
  const dashdash = argv.indexOf('--');
  const plain = dashdash === -1 ? argv : argv.slice(0, dashdash);
  const extraArgs = dashdash === -1 ? [] : argv.slice(dashdash + 1);
  const [id] = plain;
  if (!id) return fail('用法：run <id> [-- 脚本参数...]');
  let projectDir;
  try {
    projectDir = requireProject(ctx, id);
  } catch (error) {
    return fail(error.message);
  }
  const specPath = join(projectDir, 'run.json');
  let specRaw;
  try {
    specRaw = readFileSync(specPath, 'utf8');
  } catch {
    return fail('项目缺少 run.json（试运行声明：{"pkg": "tsx@4.19.2", "script": "tools/main.ts"}）');
  }
  let spec;
  try {
    spec = JSON.parse(specRaw);
  } catch (error) {
    return fail(`run.json 解析失败：${error.message}`);
  }
  if (!/^[a-z0-9@/._-]+@\d+\.\d+\.\d+$/.test(spec.pkg ?? '')) {
    return fail(`run.json pkg 必须为 <name>@<精确版本>（禁范围符号与 latest）：${spec.pkg}`);
  }
  if (typeof spec.script !== 'string' || spec.script.trim() === '') {
    return fail('run.json script 不能为空');
  }
  let scriptPath;
  try {
    scriptPath = resolveInside(projectDir, spec.script, 'run.json script');
    // 逐级断言无符号链接：path.resolve 是纯字符串运算不解析链接，
    // 必须 lstat 逐级检查，确保执行的是项目内真实文件。
    assertNoSymlinkPath(projectDir, scriptPath);
  } catch (error) {
    return fail(error.message);
  }
  let scriptStat;
  try {
    scriptStat = lstatSync(scriptPath);
  } catch {
    return fail(`run.json script 不存在：${spec.script}`);
  }
  if (!scriptStat.isFile()) {
    return fail(`run.json script 必须是普通文件：${spec.script}`);
  }
  for (const arg of extraArgs) {
    if (arg.length > 512 || arg.startsWith('/') || arg.split('/').includes('..')) {
      return fail(`试运行参数含非法值：${arg}`);
    }
  }
  const argvList = ['-y', spec.pkg, scriptPath, ...extraArgs];
  const commandDisplay = `npx ${argvList.slice(1).join(' ')}`;
  mkdirSync(join(projectDir, 'logs'), { recursive: true });
  appendFileSync(
    join(projectDir, 'logs', 'run.log'),
    `\n# 运行 ${new Date().toISOString().slice(0, 19).replace('T', ' ')} ${commandDisplay}\n`,
  );

  const started = Date.now();
  const { code, stdout, stderr, timedOut, spawnError } = await new Promise((resolvePromise) => {
    let settled = false;
    const settle = (value) => {
      if (!settled) {
        settled = true;
        resolvePromise(value);
      }
    };
    let child;
    try {
      child = spawn('npx', argvList, spawnOptions({
        cwd: projectDir,
        env: { ...process.env, npm_config_cache: join(projectDir, 'runtime', 'npm-cache') },
      }));
    } catch (error) {
      settle({ code: -1, stdout: '', stderr: '', timedOut: false, spawnError: error.message });
      return;
    }
    let stdout = '';
    let stderr = '';
    child.stdout?.on('data', (chunk) => {
      stdout += chunk;
    });
    child.stderr?.on('data', (chunk) => {
      stderr += chunk;
    });
    const timedOutRef = { value: false };
    const timer = setTimeout(() => {
      timedOutRef.value = true;
      killTree(child);
      // 终止进程树后等 close 事件完成 Promise（等待真正退出）。
    }, RUN_TIMEOUT_MS);
    child.on('close', (code) => {
      clearTimeout(timer);
      settle({ code: code ?? -1, stdout, stderr, timedOut: timedOutRef.value, spawnError: null });
    });
    child.on('error', (error) => {
      clearTimeout(timer);
      settle({ code: -1, stdout, stderr: `${stderr}\n${error.message}`, timedOut: false, spawnError: error.message });
    });
  });
  if (spawnError && code === -1 && !stdout && !stderr.includes('退出')) {
    return { ok: false, exit_code: -1, timed_out: false, duration_ms: Date.now() - started,
      command: commandDisplay, stdout_tail: '', stderr_tail: `启动 npx 失败：${spawnError}`,
      error: '未找到 npx。试运行需要 Node ≥ 20（自带 npx），请安装后重试：https://nodejs.org' };
  }
  const durationMs = Date.now() - started;
  appendFileSync(join(projectDir, 'logs', 'run.log'), `# 退出码 ${code}（${durationMs} ms）\n`);
  return {
    ok: !timedOut && code === 0,
    exit_code: code,
    timed_out: timedOut,
    duration_ms: durationMs,
    command: commandDisplay,
    stdout_tail: tailText(stdout),
    stderr_tail: tailText(stderr),
  };
}

function tailText(text) {
  const buffer = Buffer.from(text, 'utf8');
  if (buffer.length <= OUTPUT_TAIL) return text;
  return buffer.subarray(buffer.length - OUTPUT_TAIL).toString('utf8');
}
