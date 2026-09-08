// OpenAI 生图设置页脚本（Shadow 容器注入 bridge 风格）。
// bridge 由宿主容器注入执行；主题经宿主同名 CSS 变量穿透继承，无需脚本处理。

// 配置读写经宿主桥接转发到 WASM 逻辑层（plugin.* → handle_view_message）。
async function callHost(method, payload = "") {
  return bridge.call(`plugin.${method}`, payload);
}

// ── DOM ──

// Shadow 容器的页面 DOM 挂在 shadow root（宿主注入 pluginRoot），脚本里的
// document 是主文档——必须从 pluginRoot 查询，直接打开时回退 document。
const dom = typeof pluginRoot !== "undefined" && pluginRoot ? pluginRoot : document;
const byId = (id) => dom.querySelector(`#${id}`);

const sourceGlobal = byId("source-global");
const sourceManual = byId("source-manual");
const globalSection = byId("global-section");
const manualSection = byId("manual-section");
const globalModel = byId("global-model");
const globalHint = byId("global-hint");
const manualBaseUrl = byId("manual-base-url");
const manualApiKey = byId("manual-api-key");
const manualModel = byId("manual-model");
const extraPrompt = byId("extra-prompt");
const saveBtn = byId("save-btn");
const statusEl = byId("status");

function toggleSource() {
  const manual = sourceManual.checked;
  globalSection.hidden = manual;
  manualSection.hidden = !manual;
}

sourceGlobal.addEventListener("change", toggleSource);
sourceManual.addEventListener("change", toggleSource);

function setStatus(message, type) {
  statusEl.textContent = message;
  statusEl.className = `status${type ? ` ${type}` : ""}`;
}

// ── 加载 ──

async function loadConfig() {
  try {
    const raw = await callHost("bootstrap", "{}");
    const data = raw ? JSON.parse(raw) : {};
    const config = data.config || {};
    const models = data.models || [];

    // 填充模型下拉
    globalModel.innerHTML = "";
    if (models.length === 0) {
      globalModel.innerHTML = '<option value="">（暂无已配置的 chat 模型）</option>';
    } else {
      const placeholder = document.createElement("option");
      placeholder.value = "";
      placeholder.textContent = "请选择模型";
      globalModel.appendChild(placeholder);
      models.forEach((m) => {
        const opt = document.createElement("option");
        opt.value = m.key;
        const status = m.configured ? "✓" : "未配置";
        opt.textContent = `${m.key} — ${m.model} (${status})`;
        globalModel.appendChild(opt);
      });
    }

    // 回显配置
    const isManual = config.source === "manual";
    sourceManual.checked = isManual;
    sourceGlobal.checked = !isManual;
    toggleSource();

    if (config.global_model_key) globalModel.value = config.global_model_key;
    if (config.manual_endpoint) {
      manualBaseUrl.value = config.manual_endpoint.base_url || "";
      manualApiKey.value = config.manual_endpoint.api_key || "";
      manualModel.value = config.manual_endpoint.model || "";
    }
    extraPrompt.value = config.extra_prompt || "";

    globalHint.textContent = models.length > 0
      ? `共 ${models.length} 个 chat 模型可选`
      : "请先在设置中配置 chat 模型，或改用手动输入";
  } catch (error) {
    setStatus(`加载失败：${error.message || error}`, "error");
  }
}

// ── 保存 ──

async function saveConfig() {
  saveBtn.disabled = true;
  setStatus("保存中...", "");
  try {
    const payload = {
      source: sourceManual.checked ? "manual" : "global",
      global_model_key: sourceGlobal.checked ? (globalModel.value || null) : null,
      manual_endpoint: {
        base_url: manualBaseUrl.value.trim(),
        api_key: manualApiKey.value,
        model: manualModel.value.trim(),
      },
      extra_prompt: extraPrompt.value.trim() || null,
    };
    await callHost("save_config", JSON.stringify(payload));
    setStatus("已保存", "success");
    setTimeout(() => setStatus("", ""), 3000);
  } catch (error) {
    setStatus(`保存失败：${error.message || error}`, "error");
  } finally {
    saveBtn.disabled = false;
  }
}

saveBtn.addEventListener("click", saveConfig);
loadConfig();
