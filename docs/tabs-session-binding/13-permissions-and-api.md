# 13 - Tauri API 与权限声明

## 目标

补齐新增 Tauri 命令的前端 API 封装和权限声明。

## 范围

- `frontend/src/api/tauri.ts`
- `crates/plugins/tiangong-plugin-browser/build.rs`
- `crates/plugins/tiangong-plugin-browser/permissions/**`
- `crates/plugins/tiangong-plugin-terminal/build.rs`
- `crates/plugins/tiangong-plugin-terminal/permissions/**`
- `src-tauri/capabilities/default.json` 如有需要

## 依赖

- 前置任务：02、03、06。
- 后续任务：10、11、12、14。
- 可并行任务：04、05、08、09。
- 阻塞说明：权限和 API 封装需要新增命令名称和参数基本稳定后再统一补齐。

## 任务

- 为新增浏览器命令生成权限。
- 为新增终端命令生成权限。
- 更新默认权限集合。
- 前端 API 中补齐类型签名。
- 命令命名统一使用插件前缀：
  - `plugin:browser|...`
  - `plugin:terminal|...`

## 不做

- 不实现业务逻辑。
- 不改 UI。

## 验收

- 前端调用新增命令不因权限缺失失败。
- 权限 reference/schema 与命令列表一致。

## 验证

- `cargo fmt -- --check`
- `cargo check --workspace`
- `yarn build`
