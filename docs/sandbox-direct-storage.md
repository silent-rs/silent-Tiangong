# Sandbox 直存与生产信任边界

## 需求

1. 天工存储根下的 Sandbox 使用固定直存布局：

```text
<storage>/sandbox/tiangong-sandbox[.exe]
<storage>/sandbox/tiangong-sandbox[.exe].sig
```

2. 不创建 `active`、`pending` 或 `versions/<版本>/` 仓库；版本由程序 `--self-check` 自报。
3. 定位阶段只接受普通程序与普通签名文件，拒绝符号链接。
4. 生产 Sandbox 必须通过 `tiangong-sandbox` crate 内置的独立官方公钥验签；插件用户密钥和第三方插件信任根不得参与。
5. 官方验签后必须执行真实自检，并确认协议与策略 Schema 与当前宿主一致。
6. Sandbox 下载、首次安装和自更新继续复用现有 `SelfUpdater`，不在本分支实现 App 前端、Tauri 命令或 Sidecar 接线。

## 非目标

1. 不迁移旧版本目录；离线迁移另行处理。
2. 不增加最低产品版本门槛；由后续兼容任务处理。
3. 不修改终端跨会话路由、Sidecar 策略、设置页和启动准备页。

## 完成标准

- 直存路径解析和符号链接拒绝有测试覆盖。
- 用户插件密钥无法验证生产 Sandbox；生产验证入口只使用 Sandbox 官方根。
- Sandbox crate 格式、构建、测试和严格 lint 通过。
