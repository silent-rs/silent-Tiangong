// 索引管理设置页脚本。
//
// 与 memory.js 同构的 host context + callHost 桥接框架：
// - 启动时发 plugin_host_ready，等待宿主回传 host context（主题 + CSS token + channel）
// - callHost(method, payload) 经 postMessage 把请求发回宿主，宿主调 pluginCall 转发到 WASM
//   的 handle_view_message，结果回传（天工不解析消息内容，只做透传）

// 与 sidecar request_timeout_ms（60s）对齐，留出 IPC 通信余量。
// 大工作区重建耗时较长，30s 会先于 sidecar 失败导致前端误报。
const HOST_TIMEOUT_MS = 90000;
let hostChannel = null;
let hostReadyResolve = null;
const hostReady = new Promise((resolve) => {
  hostReadyResolve = resolve;
});
let requestSequence = 0;

function applyHostContext(context) {
  if (hostChannel && context.channel !== hostChannel) return;
  hostChannel = context.channel;
  const root = document.documentElement;
  root.dataset.theme = context.theme === "dark" ? "dark" : "light";
  Object.entries(context.tokens || {}).forEach(([name, value]) => {
    if (typeof value === "string" && value) {
      root.style.setProperty(`--host-${name}`, value);
    }
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
    const id = `index-${Date.now()}-${++requestSequence}`;
    const channel = hostChannel;
    const timeout = window.setTimeout(() => {
      window.removeEventListener("message", handler);
      reject(new Error("插件请求超时"));
    }, HOST_TIMEOUT_MS);
    const handler = (event) => {
      if (
        event.source !== window.parent ||
        !event.data ||
        event.data.id !== id ||
        event.data.channel !== channel
      )
        return;
      window.clearTimeout(timeout);
      window.removeEventListener("message", handler);
      if (event.data.error) {
        reject(new Error(String(event.data.error)));
      } else {
        resolve(event.data.result ?? "");
      }
    };
    window.addEventListener("message", handler);
    window.parent.postMessage(
      { type: "plugin_call", channel, id, method, payload },
      "*",
    );
  });
}

// ── 索引管理业务 ──

const listEl = document.getElementById("list");
const statusEl = document.getElementById("status");
const emptyTemplate = document.getElementById("empty-template");
const rowTemplate = document.getElementById("row-template");

function setStatus(text, isError = false) {
  statusEl.textContent = text;
  statusEl.classList.toggle("error", isError);
}

async function loadIndexes() {
  setStatus("加载中…");
  try {
    const raw = await callHost("list", "{}");
    const items = raw ? JSON.parse(raw) : [];
    renderList(Array.isArray(items) ? items : []);
    setStatus("");
  } catch (e) {
    renderList([]);
    setStatus(`加载索引列表失败：${e.message || e}`, true);
  }
}

function renderList(items) {
  listEl.innerHTML = "";
  if (items.length === 0) {
    listEl.appendChild(emptyTemplate.content.cloneNode(true));
    return;
  }
  for (const item of items) {
    const node = rowTemplate.content.firstElementChild.cloneNode(true);
    node.querySelector(".index-row-root").textContent = item.root || item.id;
    node.querySelector(".index-row-count").textContent = `${item.entry_count ?? 0} 个文件`;
    node.querySelector(".index-row-updated").textContent = item.updated_at || "";
    const rebuildBtn = node.querySelector(".rebuild-btn");
    const deleteBtn = node.querySelector(".delete-btn");
    rebuildBtn.addEventListener("click", () => rebuild(item, rebuildBtn));
    deleteBtn.addEventListener("click", () => remove(item, deleteBtn));
    listEl.appendChild(node);
  }
}

async function rebuild(item, btn) {
  btn.disabled = true;
  setStatus(`重建中：${item.root || item.id} …`);
  try {
    await callHost("rebuild", JSON.stringify({ root: item.root }));
    setStatus("重建完成");
    await loadIndexes();
  } catch (e) {
    setStatus(`重建失败：${e.message || e}`, true);
  } finally {
    btn.disabled = false;
  }
}

async function remove(item, btn) {
  btn.disabled = true;
  setStatus(`删除中：${item.root || item.id} …`);
  try {
    await callHost(
      "delete",
      JSON.stringify({ root: item.root, workspace_id: item.id }),
    );
    setStatus("已删除");
    await loadIndexes();
  } catch (e) {
    setStatus(`删除失败：${e.message || e}`, true);
  } finally {
    btn.disabled = false;
  }
}

document.getElementById("refresh-btn").addEventListener("click", loadIndexes);

// 挂载即加载列表。
loadIndexes();
