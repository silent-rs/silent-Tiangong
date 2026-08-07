// Prompt 设置页脚本（与 skill/scheduler 同构的桥接框架）。

const HOST_TIMEOUT_MS = 60000;
let hostChannel = null;
let hostReadyResolve = null;
const hostReady = new Promise((resolve) => { hostReadyResolve = resolve; });
let requestSequence = 0;

function applyHostContext(context) {
  if (hostChannel && context.channel !== hostChannel) return;
  hostChannel = context.channel;
  const root = document.documentElement;
  root.dataset.theme = context.theme === "dark" ? "dark" : "light";
  Object.entries(context.tokens || {}).forEach(([name, value]) => {
    if (typeof value === "string" && value) root.style.setProperty(`--host-${name}`, value);
  });
  if (typeof context.fontFamily === "string" && context.fontFamily) {
    root.style.setProperty("--host-font-family", context.fontFamily);
  }
  hostReadyResolve?.();
  hostReadyResolve = null;
}

window.addEventListener("message", (event) => {
  if (event.source !== window.parent || !event.data) return;
  if (event.data.type === "tiangong_host_context" && typeof event.data.channel === "string") {
    applyHostContext(event.data);
  }
});
window.parent.postMessage({ type: "plugin_host_ready" }, "*");

async function callHost(method, payload = "") {
  if (!hostChannel) await hostReady;
  return new Promise((resolve, reject) => {
    const id = `prompt-${Date.now()}-${++requestSequence}`;
    const channel = hostChannel;
    const timeout = window.setTimeout(() => {
      window.removeEventListener("message", handler);
      reject(new Error("插件请求超时"));
    }, HOST_TIMEOUT_MS);
    const handler = (event) => {
      if (event.source !== window.parent || !event.data || event.data.id !== id || event.data.channel !== channel) return;
      window.clearTimeout(timeout);
      window.removeEventListener("message", handler);
      if (event.data.error) reject(new Error(String(event.data.error)));
      else resolve(event.data.result ?? "");
    };
    window.addEventListener("message", handler);
    window.parent.postMessage({ type: "plugin_call", channel, id, method, payload }, "*");
  });
}

// ── DOM ──

const editor = document.getElementById("prompt-editor");
const saveBtn = document.getElementById("save-btn");
const statusEl = document.getElementById("status");

function setStatus(message, type) {
  statusEl.textContent = message;
  statusEl.className = `status${type ? ` ${type}` : ""}`;
}

// ── 加载 ──

async function loadPrompt() {
  try {
    const raw = await callHost("get_prompt", "{}");
    const data = raw ? JSON.parse(raw) : {};
    editor.value = data.content || "";
  } catch (error) {
    setStatus(`加载失败：${error.message || error}`, "error");
  }
}

// ── 保存 ──

async function savePrompt() {
  saveBtn.disabled = true;
  setStatus("保存中...", "");
  try {
    await callHost("set_prompt", JSON.stringify({ content: editor.value }));
    setStatus("已保存", "success");
    setTimeout(() => setStatus("", ""), 3000);
  } catch (error) {
    setStatus(`保存失败：${error.message || error}`, "error");
  } finally {
    saveBtn.disabled = false;
  }
}

saveBtn.addEventListener("click", savePrompt);
loadPrompt();
