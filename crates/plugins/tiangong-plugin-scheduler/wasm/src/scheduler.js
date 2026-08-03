// 定时任务设置页脚本。
//
// 与 index.js / memory.js 同构的 host context + callHost 桥接框架：
// - 启动时发 plugin_host_ready，等待宿主回传 host context（主题 + CSS token + channel）
// - callHost(method, payload) 经 postMessage 把请求发回宿主，宿主调 pluginCall 转发到 WASM
//   的 handle_view_message，结果回传（天工不解析消息内容，只做透传）

const HOST_TIMEOUT_MS = 60000;
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
    const id = `scheduler-${Date.now()}-${++requestSequence}`;
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

// ── 业务：任务列表 ──

const listEl = document.getElementById("list");
const statusEl = document.getElementById("status");
const emptyTemplate = document.getElementById("empty-template");
const rowTemplate = document.getElementById("row-template");

function setStatus(text, isError = false) {
  statusEl.textContent = text;
  statusEl.classList.toggle("error", isError);
}

async function loadJobs() {
  setStatus("加载中…");
  try {
    const raw = await callHost("list", "{}");
    const data = raw ? JSON.parse(raw) : { jobs: [] };
    renderList(data.jobs || []);
    setStatus("");
  } catch (e) {
    renderList([]);
    setStatus(`加载任务列表失败：${e.message || e}`, true);
  }
}

function renderList(jobs) {
  listEl.innerHTML = "";
  if (jobs.length === 0) {
    listEl.appendChild(emptyTemplate.content.cloneNode(true));
    return;
  }
  for (const job of jobs) {
    const node = rowTemplate.content.firstElementChild.cloneNode(true);
    node.querySelector(".job-row-name").textContent = job.name || job.id;
    node.querySelector(".job-row-schedule").textContent = job.schedule || "—";
    const statusBadge = node.querySelector(".job-row-status");
    statusBadge.textContent = job.enabled ? "启用" : "停用";
    statusBadge.classList.add(job.enabled ? "badge-success" : "badge-disabled");
    node.querySelector(".job-row-desc").textContent = job.description || "";
    node.dataset.job = JSON.stringify(job);

    const toggleBtn = node.querySelector(".toggle-btn");
    toggleBtn.textContent = job.enabled ? "停用" : "启用";
    toggleBtn.addEventListener("click", () => toggleJob(job, toggleBtn));

    node.querySelector(".trigger-btn").addEventListener("click", () => triggerJob(job));
    node.querySelector(".runs-btn").addEventListener("click", () => showRuns(job.id));
    node.querySelector(".edit-btn").addEventListener("click", () => openEditForm(job));
    node.querySelector(".delete-btn").addEventListener("click", () => deleteJob(job));

    listEl.appendChild(node);
  }
}

// ── 业务：创建/编辑 ──

const formModal = document.getElementById("form-modal");
const formTitle = document.getElementById("form-title");
const jobForm = document.getElementById("job-form");
const fieldId = document.getElementById("field-id");
const fieldName = document.getElementById("field-name");
const fieldDescription = document.getElementById("field-description");
const fieldSchedule = document.getElementById("field-schedule");
const fieldPayload = document.getElementById("field-payload");
const fieldSessionId = document.getElementById("field-session-id");
const fieldEnabled = document.getElementById("field-enabled");
const cronHint = document.getElementById("cron-hint");
const formSubmit = document.getElementById("form-submit");

document.getElementById("create-btn").addEventListener("click", () => openCreateForm());
document.getElementById("form-close").addEventListener("click", closeForm);
document.getElementById("form-cancel").addEventListener("click", closeForm);

function openCreateForm() {
  formTitle.textContent = "创建任务";
  fieldId.value = "";
  fieldName.value = "";
  fieldDescription.value = "";
  fieldSchedule.value = "0 0 9 * * *";
  fieldPayload.value = "";
  fieldSessionId.value = "";
  fieldEnabled.checked = true;
  validateCronInput();
  formModal.hidden = false;
}

function openEditForm(job) {
  formTitle.textContent = "编辑任务";
  fieldId.value = job.id;
  fieldName.value = job.name || "";
  fieldDescription.value = job.description || "";
  fieldSchedule.value = job.schedule || "";
  fieldPayload.value = job.payload || "";
  fieldSessionId.value = job.session_id || "";
  fieldEnabled.checked = job.enabled !== false;
  validateCronInput();
  formModal.hidden = false;
}

function closeForm() {
  formModal.hidden = true;
}

// 简单 Cron 校验：要求 6 个字段（秒 分 时 日 月 周）。
fieldSchedule.addEventListener("input", validateCronInput);

function validateCronInput() {
  const value = fieldSchedule.value.trim();
  const parts = value.split(/\s+/).filter(Boolean);
  if (parts.length === 0) {
    cronHint.textContent = "6 字段：秒 分 时 日 月 周";
    cronHint.classList.remove("error");
    return true;
  }
  if (parts.length !== 6 && parts.length !== 7) {
    cronHint.textContent = `需 6~7 字段，当前 ${parts.length} 个`;
    cronHint.classList.add("error");
    return false;
  }
  cronHint.textContent = "6 字段：秒 分 时 日 月 周";
  cronHint.classList.remove("error");
  return true;
}

jobForm.addEventListener("submit", async (e) => {
  e.preventDefault();
  if (!validateCronInput()) return;
  const name = fieldName.value.trim();
  const description = fieldDescription.value.trim();
  const schedule = fieldSchedule.value.trim();
  const payload = fieldPayload.value.trim();
  if (!name || !description || !schedule || !payload) {
    setStatus("请填写所有必填字段", true);
    return;
  }
  const sessionId = fieldSessionId.value.trim();
  const enabled = fieldEnabled.checked;
  formSubmit.disabled = true;
  formSubmit.textContent = "保存中…";
  try {
    const id = fieldId.value;
    if (id) {
      await callHost("update", JSON.stringify({
        id,
        name,
        description,
        schedule,
        session_id: sessionId || null,
        payload,
        enabled,
      }));
      setStatus("已更新");
    } else {
      await callHost("create", JSON.stringify({
        name,
        description,
        schedule,
        session_id: sessionId || null,
        payload,
        enabled,
      }));
      setStatus("已创建");
    }
    closeForm();
    await loadJobs();
  } catch (err) {
    setStatus(`保存失败：${err.message || err}`, true);
  } finally {
    formSubmit.disabled = false;
    formSubmit.textContent = "保存";
  }
});

// ── 业务：启用/停用 ──

async function toggleJob(job, btn) {
  btn.disabled = true;
  try {
    await callHost("update", JSON.stringify({ id: job.id, enabled: !job.enabled }));
    setStatus(job.enabled ? "已停用" : "已启用");
    await loadJobs();
  } catch (e) {
    setStatus(`操作失败：${e.message || e}`, true);
  } finally {
    btn.disabled = false;
  }
}

// ── 业务：删除 ──

async function deleteJob(job) {
  if (!confirm(`确认删除任务「${job.name || job.id}」？`)) return;
  setStatus(`删除中…`);
  try {
    await callHost("delete", JSON.stringify({ id: job.id }));
    setStatus("已删除");
    await loadJobs();
  } catch (e) {
    setStatus(`删除失败：${e.message || e}`, true);
  }
}

// ── 业务：触发 ──

async function triggerJob(job) {
  setStatus(`正在触发「${job.name || job.id}」…`);
  try {
    await callHost("trigger", JSON.stringify({ id: job.id }));
    setStatus(`已触发「${job.name || job.id}」，执行中`);
  } catch (e) {
    setStatus(`触发失败：${e.message || e}`, true);
  }
}

// ── 业务：执行历史 ──

const runsModal = document.getElementById("runs-modal");
const runsList = document.getElementById("runs-list");
const runsEmptyTemplate = document.getElementById("runs-empty-template");
const runRowTemplate = document.getElementById("run-row-template");

document.getElementById("runs-close").addEventListener("click", () => {
  runsModal.hidden = true;
});

async function showRuns(jobId) {
  runsList.innerHTML = "";
  runsModal.hidden = false;
  try {
    const raw = await callHost("runs", JSON.stringify({ id: jobId, limit: 20 }));
    const data = raw ? JSON.parse(raw) : { runs: [] };
    const runs = data.runs || [];
    if (runs.length === 0) {
      runsList.appendChild(runsEmptyTemplate.content.cloneNode(true));
      return;
    }
    for (const run of runs) {
      const node = runRowTemplate.content.firstElementChild.cloneNode(true);
      const badge = node.querySelector(".run-status");
      badge.textContent = run.status || "unknown";
      badge.classList.add(`badge-${run.status || "unknown"}`);
      node.querySelector(".run-started").textContent = run.started_at || "";
      node.querySelector(".run-summary").textContent = run.result_summary || "";
      runsList.appendChild(node);
    }
  } catch (e) {
    runsList.textContent = `加载历史失败：${e.message || e}`;
  }
}

// 挂载即加载列表。
loadJobs();
