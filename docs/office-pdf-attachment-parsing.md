# Office / PDF 文件上传与内容解析（issue #149）

## 背景

早期文件上传仅支持图片（png/jpg/jpeg/webp/gif）和少量文本格式（txt/md/json/csv）。用户经常需要让 Agent 处理 Office 文档与 PDF：

- "帮我总结这个 PDF 的内容"
- "分析这个 Excel 表格的数据"
- "把这份 PPT 的要点提取出来"

本文档说明本期支持的格式范围、解析策略与架构决策。

## 支持的格式

| 格式 | 扩展名 | MIME |
|---|---|---|
| PDF | `.pdf` | `application/pdf` |
| Word | `.docx` | `application/vnd.openxmlformats-officedocument.wordprocessingml.document` |
| Excel | `.xlsx` | `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet` |
| PowerPoint | `.pptx` | `application/vnd.openxmlformats-officedocument.presentationml.presentation` |

旧版二进制 Office（`.doc`/`.xls`/`.ppt`）本期不做，留作后续 P2。

## 核心设计理念：多模态优先 + Agent 自主 fallback

本系统**不在后端写死 Rust 解析器**，而是采用两层策略：

1. **多模态优先**：让模型直接读取文档原生内容
   - PDF 走 Anthropic `document` block / OpenAI Responses `input_file` 原生 API
   - Office 文件因多模态原生 API 支持有限，主要靠本地转换后喂模型
2. **Agent 自主 fallback**：把"如何用 python3/node 解析这些格式"的知识嵌入 system prompt，
   当多模态不可用或失败时，由 agent 自主调用 `run_command` 执行脚本解析

这与项目既有的"Skill 引导 agent 调脚本"模式一致，不引入新的解析框架。

## 数据流

```
用户上传 PDF/docx/xlsx/pptx
        │
        ▼
[前端] 文件选择器扩展 + MIME 推断（attachments.ts / MessageInput.tsx）
        │
        ▼ data URL / 本地路径
[归档] media_archive.rs：Office/PDF 归档到 ~/.tiangong/media/files/
        │
        ▼ ContentBlock::Media{kind:File, url:<归档路径>, mime_type}
[注入决策] 两条路径（复用现有机制）
        │
        ├─ 路径A: chat 模型自带 multimodal
        │     └─ provider_message_from_session → 随主请求发送
        │           ├─ Anthropic: document block（PDF 原生）✅
        │           └─ OpenAI Responses: input_file 🔧（本期新增）
        │
        └─ 路径B: chat 非 multimodal，有独立 multimodal 端点
              └─ analyze_attachment 工具 → multimodal_client 解析
        │
        ▼
[fallback 引导] System Prompt 注入"文档附件解析规则"段
        └─ agent 按需用 run_command 跑：
              python3 + pdfplumber / python-docx / openpyxl / python-pptx
              缺库时 pip install --target ~/.tiangong/parsers/python <pkg>
```

## 关键改动点

### 前端

- `frontend/src/utils/attachments.ts` — `fileMimeType()` 扩展 docx/xlsx/pptx
- `frontend/src/components/MessageInput.tsx` — 文件选择器 `extensions` 追加新格式

### Tauri 壳

- `src-tauri/src/commands.rs` — `mime_type_from_path()` 扩展 docx/xlsx/pptx

### 归档

- `crates/tiangong-core/src/media_archive.rs` — 新增文档归档逻辑：
  - `is_document_asset()` / `archive_file_reference()` 处理 PDF/Office
  - 文档归档到独立的 `~/.tiangong/media/files/` 子目录（与 `images/` 并列）
  - 图片归档逻辑保持不变

### LLM 注入

- `crates/tiangong-core/src/model.rs`：
  - 新增 `file_content_from_reference()`：本地路径读字节转 base64 data URL
  - 修复 `provider_message_from_session` 的 `MediaKind::File` 分支，支持本地路径注入
  - 新增 `file_mime_from_path()` 按扩展名推断文档 MIME

### Provider 原生支持

- `crates/tiangong-llm/src/providers/openai/mapping.rs`：
  - `build_user_item()` 新增 `MessageContent::File` 分支 → `input_file` 原生 block
  - `collect_text()` 移除 File 降级为文本的行为（避免 base64 膨胀 token）
- Anthropic 路径无需改动（`anthropic/mapping.rs` 已映射为 document block）
- OpenAI Chat Completions / DeepSeek：文件原生支持弱，维持现状靠 prompt fallback

### System Prompt

- `crates/tiangong-core/src/prompt/attachment_rules.rs` — **新增**：`attachment_rules_section()`
- `crates/tiangong-core/src/prompt/sections.rs` — `SystemPromptConfig` 新增 `attachment_rules_text` 字段，
  在 `collect_dynamic_parts()` 中于 media_text 之后注入

## 解析依赖隔离

Agent 自主安装解析依赖时，统一使用隔离目录，避免污染系统环境：

```
~/.tiangong/parsers/python/   # pip install --target
~/.tiangong/parsers/node/     # npm install --prefix
```

调用时设：

```
PYTHONPATH=~/.tiangong/parsers/python python3 -c "..."
```

## 非目标

- ❌ 旧版二进制 Office（`.doc`/`.xls`/`.ppt`）
- ❌ Office 文件可视化页面预览（渲染为页面/幻灯片图片）
- ❌ Office 文件编辑能力
- ❌ OCR（扫描版 PDF / 图片中的文字识别）
- ❌ 后端写死的 Rust 解析器（改为 agent 自主跑脚本）

## 安全与限制

- 文件大小限制沿用附件既有规则（base64 ≤ 50MB，见 `MAX_ATTACHMENT_BASE64_BYTES`）
- `run_command` 默认 30s 超时；agent 可用 `__tiangong_timeout=120000` 覆盖大文件解析
- 非 FullTrust 模式下，解析输出需写到工作区或 `~/.tiangong/`（均在允许根内）
- `python3`/`pip`/`node`/`npm` 均已在 `run_command` 白名单内
