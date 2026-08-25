// validate：清单与结构校验（plugin.json 解析、必要字段、UI 入口存在性）。
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { fail, requireProject } from './common.mjs';

export async function validate(argv, ctx) {
  const [id] = argv;
  if (!id) return fail('用法：validate <id>');
  let projectDir;
  try {
    projectDir = requireProject(ctx, id);
  } catch (error) {
    return fail(error.message);
  }
  const errors = [];
  const warnings = [];
  const manifestPath = join(projectDir, 'plugin.json');
  if (!existsSync(manifestPath)) {
    return { ok: false, errors: [`缺少 plugin.json`], warnings };
  }
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(manifestPath, 'utf8'));
  } catch (error) {
    return { ok: false, errors: [`plugin.json 不是有效 JSON：${error.message}`], warnings };
  }
  if (manifest.schema_version !== 2) errors.push(`schema_version 应为 2（收到 ${manifest.schema_version}）`);
  if (!validIdField(manifest.id)) errors.push(`id 非法：${manifest.id}（字母数字与 - _ .）`);
  if (!manifest.version) errors.push('version 为空');
  if (manifest.id && manifest.id !== id) {
    errors.push(`plugin.json id（${manifest.id}）与项目 ID（${id}）不一致`);
  }
  const contributions = manifest.ui?.contributions ?? [];
  for (const contribution of contributions) {
    const entry = contribution.entry ?? '';
    if (!entry) {
      errors.push('ui.contributions 缺少 entry');
      continue;
    }
    if (!existsSync(join(projectDir, entry))) {
      warnings.push(`UI 入口 ${entry} 尚不存在（通常表示还未构建）`);
    }
  }
  if (contributions.length === 0 && !manifest.tools?.length && !manifest.wasm) {
    warnings.push('清单未声明任何 UI 贡献、工具或逻辑层，安装后不会有可见效果');
  }
  const sidecar = manifest.sidecar;
  if (sidecar && !(manifest.permissions ?? []).includes('sidecar.invoke')) {
    errors.push('声明 sidecar 时必须声明 sidecar.invoke 权限');
  }
  if (sidecar?.runtime && sidecar.runtime !== 'npx') {
    errors.push(`sidecar.runtime 非法值 ${sidecar.runtime}（仅支持 npx）`);
  }
  return {
    ok: errors.length === 0,
    errors,
    warnings,
    id: manifest.id,
    version: manifest.version,
    permissions: manifest.permissions ?? [],
    tools: (manifest.tools ?? []).map((tool) => tool.name),
  };
}

function validIdField(id) {
  return typeof id === 'string' && /^[A-Za-z0-9._-]+$/.test(id) && id !== '.' && id !== '..';
}
