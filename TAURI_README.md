# 天工桌面应用 - Tauri 版本

## 项目结构

```
tiangong/
├── src/                    # 核心 Rust 代码库
│   ├── core/              # 核心业务逻辑
│   ├── cli/               # CLI 模式
│   └── lib.rs             # 库入口
├── src-tauri/             # Tauri 后端
│   ├── src/
│   │   ├── main.rs        # Tauri 入口
│   │   ├── commands.rs    # Tauri Commands (API)
│   │   ├── types.rs       # 前后端共享类型
│   │   └── app.rs         # 应用状态
│   ├── Cargo.toml
│   └── tauri.conf.json
└── frontend/              # React 前端
    ├── src/
    │   ├── api/          # Tauri API 封装
    │   ├── store/        # Zustand 状态管理
    │   ├── components/   # UI 组件
    │   └── pages/        # 页面组件
    ├── package.json
    └── vite.config.ts
```

## 开发环境准备

### 1. 安装 Rust

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

### 2. 安装 Node.js 和 Yarn

```bash
# 使用 nvm 安装 Node.js
nvm install 20
nvm use 20

# 安装 Yarn
npm install -g yarn
```

### 3. 安装 Tauri CLI

```bash
cargo install tauri-cli --version "^2.0.0"
```

## 开发模式运行

### 启动开发服务器

```bash
# 在项目根目录
yarn install        # 安装前端依赖
cargo tauri dev     # 启动 Tauri 开发模式
```

这将：
1. 启动 Vite 开发服务器 (http://localhost:5173)
2. 编译 Rust 后端
3. 打开 Tauri 窗口

### 热重载

- 前端代码更改会自动热重载
- Rust 代码更改会自动重新编译

## 构建生产版本

```bash
cargo tauri build
```

构建产物位于 `src-tauri/target/release/bundle/`

## 功能清单

### 已实现

- ✅ 会话管理（创建、切换、删除）
- ✅ 消息发送和流式输出
- ✅ 计划展示（可折叠步骤）
- ✅ 代码高亮
- ✅ 思考过程折叠
- ✅ 状态指示器
- ✅ 深色主题（参考 Codex）

### 待实现

- ⏳ MCP 服务器管理对话框
- ⏳ Skill 管理对话框
- ⏳ 模型选择器
- ⏳ 输入历史
- ⏳ 消息操作（复制、重新生成）
- ⏳ 文件变更展示（DiffViewer）

## 常见问题

### 端口冲突

如果 5173 端口被占用，修改 `frontend/vite.config.ts` 中的 `server.port` 和 `src-tauri/tauri.conf.json` 中的 `devUrl`。

### Rust 编译错误

确保 Rust 版本 >= 1.80：
```bash
rustup update
```

### 前端依赖问题

删除 `node_modules` 和重新安装：
```bash
cd frontend
rm -rf node_modules yarn.lock
yarn install
```

## 开发规范

- 使用简体中文回复和注释
- 遵循 ESLint 和 Rust Clippy 规则
- 提交前运行 `cargo fmt` 和 `cargo clippy`
