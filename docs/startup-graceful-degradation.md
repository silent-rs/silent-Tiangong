# 启动准备失败降级放行

- 日期：2026-09-15
- 分支：`feature/startup-graceful-degradation`
- 背景：用户设备上 plugin-creator 插件因目录权限异常（写入 ACL os error 5）启动失败，
  旧版启动门闸以"运行环境准备失败"整页阻断进入应用，导致对话功能也不可用。

## 需求

1. 插件异常（验证失败、常驻进程启动失败等）时，将该插件标记为异常状态，
   不得阻断进入应用；
2. 沙箱程序不可用（安装失败、验签失败等）时同样允许进入应用，
   仅插件工具不可用；
3. 与 agent 的对话功能在任何启动准备失败下均可用（对话不依赖插件与沙箱）。

## 行为设计

| 场景 | 旧行为 | 新行为 |
| --- | --- | --- |
| 某插件验证/启动失败 | 整页阻断，仅重试/退出 | 正常进入；插件标记异常（设置页可见），工具调用时报各自原因 |
| 沙箱程序安装/验证失败 | 整页阻断 | 正常进入；横幅提示，设置页"沙箱管理"可修复 |
| 对话 | 进不去则不可用 | 始终可用 |

安全边界不变：沙箱起不来时 sidecar 照样拒绝启动（fail-closed，不降级为
无沙箱执行），只是影响范围从"整个应用"收缩到"该插件的功能"。

## 实现要点

- `registry::prepare_desktop_startup_plugins` 返回 `StartupPluginReadiness`
  （`loaded` + `failures`），插件级失败逐个写入既有的 `runtime_error` /
  无效插件登记后仅汇总返回，不再 `bail!` 整体报错；
- `prepare_startup_resources`（Tauri 命令）沙箱安装失败改为返回
  `degraded_reason`，插件预加载失败转为 `plugin_failures` 清单，均不抛错；
- 前端 `StartupPrepareGate` 仅在"准备中"显示等待页；失败态放行进入主界面，
  顶部可关闭横幅分别提示沙箱不可用与插件失败数量，指向设置页修复入口；
- 修复自愈：启动时沙箱装好后重试失败插件（既有逻辑），设置页沙箱
  "检查并更新"成功后新增 `retry_failed_plugin_preload()` 自动重试；
- 设置页插件列表异常展示为既有能力（`last_error` + error/degraded 状态），未改动。

## 验证

- `cargo test -p tiangong-plugin-runtime --lib -- --test-threads=1`：180 通过
  （含改写的启动准备降级与恢复两用例）；
- `cargo test -p tiangong-app --lib`：63 通过；
- `cargo clippy -p tiangong-plugin-runtime -p tiangong-app`：无告警；
- `yarn build`：通过；
- GUI 实际降级路径（断网首装、损坏插件目录）待用户桌面实测。
