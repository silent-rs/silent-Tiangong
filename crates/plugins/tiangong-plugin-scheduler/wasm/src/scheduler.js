// 定时任务设置页脚本。
//
// 与 index.js / memory.js 同构的 host context + callHost 桥接框架。
// UI 对齐主程序原版 React 页面（JobPanel / JobFormDialog / RunHistoryDialog）。

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

function setHostMask(visible) {
  if (!hostChannel) return;
  window.parent.postMessage(
    {
      type: "plugin_host_mask",
      channel: hostChannel,
      visible,
      ...(visible ? { color: "rgba(0, 0, 0, 0.5)" } : {}),
    },
    "*",
  );
}

function syncHostMask() {
  const formOpen = !document.getElementById("form-modal").hidden;
  const runsOpen = !document.getElementById("runs-modal").hidden;
  setHostMask(formOpen || runsOpen);
}

// ── Cron 工具（简化版，不依赖外部库）──

const WEEKDAY_OPTIONS = [
  { value: "*", label: "每天", cron: "*" },
  { value: "1-5", label: "工作日（周一至周五）", cron: "1-5" },
  { value: "0,6", label: "周末", cron: "0,6" },
  { value: "1", label: "周一", cron: "1" },
  { value: "2", label: "周二", cron: "2" },
  { value: "3", label: "周三", cron: "3" },
  { value: "4", label: "周四", cron: "4" },
  { value: "5", label: "周五", cron: "5" },
  { value: "6", label: "周六", cron: "6" },
  { value: "0", label: "周日", cron: "0" },
];

function buildSimpleSchedule(minute, hour, weekdayValue) {
  const weekdayCron = WEEKDAY_OPTIONS.find((o) => o.value === weekdayValue)?.cron ?? "*";
  const mm = clamp(minute, 0, 59);
  const hh = clamp(hour, 0, 23);
  return `0 ${mm} ${hh} * * ${weekdayCron}`;
}

function tryParseToSimple(expr) {
  if (!expr) return null;
  const fields = expr.trim().split(/\s+/);
  if (fields.length !== 6) return null;
  if (fields[0] !== "0") return null;
  if (fields[3] !== "*" || fields[4] !== "*") return null;
  const minute = parseInt(fields[1], 10);
  const hour = parseInt(fields[2], 10);
  if (!Number.isInteger(minute) || !Number.isInteger(hour)) return null;
  if (minute < 0 || minute > 59 || hour < 0 || hour > 23) return null;
  if (fields[1] !== String(minute) || fields[2] !== String(hour)) return null;
  const weekday = WEEKDAY_OPTIONS.find((o) => o.cron === fields[5])?.value;
  if (!weekday) return null;
  return { minute, hour, weekday };
}

function clamp(n, min, max) {
  if (!Number.isFinite(n)) return min;
  return Math.min(max, Math.max(min, Math.trunc(n)));
}

/** 校验 6 字段 cron 表达式。返回 { ok, error? }。 */
function validateCron(expr) {
  const trimmed = (expr || "").trim();
  if (!trimmed) return { ok: false, error: "请输入 cron 表达式" };
  const fields = trimmed.split(/\s+/);
  if (fields.length < 6) {
    return { ok: false, error: `需要 6 个字段（秒 分 时 日 月 周），当前只有 ${fields.length} 个` };
  }
  if (fields.length > 6) {
    return { ok: false, error: "应为 6 个字段（秒 分 时 日 月 周）；带年的 7 字段请直接保存由后端校验" };
  }
  for (const f of fields) {
    if (!/^[0-9*/,-]+$/.test(f)) {
      return { ok: false, error: `字段「${f}」包含非法字符` };
    }
  }
  return { ok: true };
}

/**
 * 计算下次触发时间（简化版，支持基本 cron 语法）。
 * 返回接下来 count 次的 Date 数组，失败返回 null。
 */
function nextRuns(expr, count) {
  const valid = validateCron(expr);
  if (!valid.ok) return null;
  const fields = expr.trim().split(/\s+/);
  const [sec, min, hour, dom, mon, dow] = fields;
  const now = new Date();
  const results = [];
  const fixedSecond = /^\d+$/.test(sec) ? parseInt(sec, 10) : null;
  const stepMs = fixedSecond !== null && fixedSecond >= 0 && fixedSecond <= 59 ? 60000 : 1000;
  let cursor = new Date(now);
  cursor.setMilliseconds(0);
  if (stepMs === 60000) {
    cursor.setSeconds(fixedSecond);
    if (cursor <= now) cursor = new Date(cursor.getTime() + stepMs);
  } else {
    cursor = new Date(cursor.getTime() + stepMs);
  }
  let iterations = 0;
  const maxIterations = stepMs === 60000 ? 600000 : 500000;

  while (results.length < count && iterations < maxIterations) {
    iterations++;
    if (matchField(sec, cursor.getSeconds()) &&
        matchField(min, cursor.getMinutes()) &&
        matchField(hour, cursor.getHours()) &&
        matchField(dom, cursor.getDate()) &&
        matchField(mon, cursor.getMonth() + 1) &&
        matchDow(dow, cursor.getDay())) {
      results.push(new Date(cursor));
    }
    cursor = new Date(cursor.getTime() + stepMs);
  }
  return results.length > 0 ? results : null;
}

function matchField(field, value) {
  if (field === "*") return true;
  for (const part of field.split(",")) {
    if (matchPart(part, value)) return true;
  }
  return false;
}

function matchPart(part, value) {
  if (part === "*") return true;
  const stepMatch = part.match(/^\*\/(\d+)$/);
  if (stepMatch) {
    const step = parseInt(stepMatch[1], 10);
    return value % step === 0;
  }
  const rangeMatch = part.match(/^(\d+)-(\d+)$/);
  if (rangeMatch) {
    const lo = parseInt(rangeMatch[1], 10);
    const hi = parseInt(rangeMatch[2], 10);
    return value >= lo && value <= hi;
  }
  const slashMatch = part.match(/^(\d+)\/(\d+)$/);
  if (slashMatch) {
    const base = parseInt(slashMatch[1], 10);
    const step = parseInt(slashMatch[2], 10);
    return value >= base && (value - base) % step === 0;
  }
  const num = parseInt(part, 10);
  return num === value;
}

/** 周字段匹配：cron 中 0/7=周日，JS getDay() 中 0=周日。 */
function matchDow(dowField, jsDay) {
  if (dowField === "*") return true;
  const normalize = (v) => (v === 7 ? 0 : v);
  for (const part of dowField.split(",")) {
    if (part.includes("-")) {
      const [lo, hi] = part.split("-").map((s) => normalize(parseInt(s, 10)));
      if (lo <= hi) {
        if (jsDay >= lo && jsDay <= hi) return true;
      } else {
        if (jsDay >= lo || jsDay <= hi) return true;
      }
    } else {
      if (normalize(parseInt(part, 10)) === jsDay) return true;
    }
  }
  return false;
}

function formatLocal(d) {
  const pad = (n) => String(n).padStart(2, "0");
  const wd = ["周日", "周一", "周二", "周三", "周四", "周五", "周六"][d.getDay()];
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())} ${wd}`;
}

function relativeFromNow(d, ref) {
  const diffMs = d.getTime() - ref.getTime();
  const mins = Math.round(diffMs / 60000);
  if (mins < 60) return `约 ${mins} 分钟后`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `约 ${hours} 小时后`;
  const days = Math.round(hours / 24);
  return `约 ${days} 天后`;
}

function humanizeCron(expr) {
  const simple = tryParseToSimple(expr);
  if (!simple) return null;
  const weekday = WEEKDAY_OPTIONS.find((option) => option.value === simple.weekday)?.label;
  const minute = String(simple.minute).padStart(2, "0");
  const hour = String(simple.hour).padStart(2, "0");
  return weekday ? `${weekday} ${hour}:${minute}` : null;
}

// ── DOM 引用 ──

const listEl = document.getElementById("list");
const statusEl = document.getElementById("status");
const loadingState = document.getElementById("loading-state");
const jobContent = document.getElementById("job-content");
const emptyState = document.getElementById("empty-state");
const countEl = document.getElementById("job-count-num");
const refreshBtn = document.getElementById("refresh-btn");
const refreshIcon = document.getElementById("refresh-icon");
const rowTemplate = document.getElementById("row-template");

function setStatus(text, isError = false) {
  statusEl.textContent = text;
  statusEl.classList.toggle("error", isError);
  statusEl.hidden = !text;
}

// ── 任务列表 ──

async function loadJobs(manual = false) {
  refreshBtn.disabled = true;
  refreshIcon.classList.add("spinning");
  if (manual) setStatus("正在刷新…");
  try {
    const raw = await callHost("list", "{}");
    const data = raw ? JSON.parse(raw) : { jobs: [] };
    renderList(data.jobs || []);
    if (manual) setStatus("已刷新");
  } catch (e) {
    renderList([]);
    setStatus(`加载任务列表失败：${e.message || e}`, true);
  } finally {
    loadingState.hidden = true;
    jobContent.hidden = false;
    refreshBtn.disabled = false;
    refreshIcon.classList.remove("spinning");
  }
}

function renderList(jobs) {
  listEl.innerHTML = "";
  emptyState.hidden = jobs.length !== 0;
  listEl.hidden = jobs.length === 0;
  countEl.textContent = String(jobs.length);
  for (const job of jobs) {
    const node = rowTemplate.content.firstElementChild.cloneNode(true);
    node.querySelector(".job-row-name").textContent = job.name || job.id;
    node.querySelector(".job-row-name").title = job.name || job.id;
    node.querySelector(".job-row-schedule").textContent = job.schedule || "—";
    node.querySelector(".job-row-desc").textContent = job.description || "";

    const checkbox = node.querySelector(".toggle-checkbox");
    checkbox.checked = job.enabled !== false;
    checkbox.addEventListener("change", () => toggleJob(job, checkbox.checked, checkbox));

    const triggerBtn = node.querySelector(".trigger-btn");
    const deleteBtn = node.querySelector(".delete-btn");
    triggerBtn.addEventListener("click", () => triggerJob(job, triggerBtn));
    node.querySelector(".runs-btn").addEventListener("click", () => showRuns(job.id));
    node.querySelector(".edit-btn").addEventListener("click", () => openEditForm(job));
    deleteBtn.addEventListener("click", () => deleteJob(job, deleteBtn));

    listEl.appendChild(node);
  }
}

// ── 创建/编辑表单 ──

const formModal = document.getElementById("form-modal");
const formTitle = document.getElementById("form-title");
const jobForm = document.getElementById("job-form");
const fieldId = document.getElementById("field-id");
const fieldName = document.getElementById("field-name");
const fieldDescription = document.getElementById("field-description");
const fieldPayload = document.getElementById("field-payload");
const fieldSession = document.getElementById("field-session");
const formSubmit = document.getElementById("form-submit");
const formError = document.getElementById("form-error");
let formSaving = false;

// 填充星期下拉
const weekdaySelect = document.getElementById("simple-weekday");
WEEKDAY_OPTIONS.forEach((o) => {
  const opt = document.createElement("option");
  opt.value = o.value;
  opt.textContent = o.label;
  weekdaySelect.appendChild(opt);
});

let currentMode = "simple";

// Tab 切换
document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    currentMode = tab.dataset.mode;
    document.querySelectorAll(".tab").forEach((t) => t.classList.toggle("active", t === tab));
    document.getElementById("panel-simple").hidden = currentMode !== "simple";
    document.getElementById("panel-cron").hidden = currentMode !== "cron";
    updateSchedulePreview();
  });
});

const simpleMinute = document.getElementById("simple-minute");
const simpleHour = document.getElementById("simple-hour");
const fieldCron = document.getElementById("field-cron");
const simplePreview = document.getElementById("simple-preview");

[simpleMinute, simpleHour, weekdaySelect].forEach((el) => {
  el.addEventListener("input", () => {
    simplePreview.textContent = buildSimpleSchedule(
      parseInt(simpleMinute.value, 10),
      parseInt(simpleHour.value, 10),
      weekdaySelect.value,
    );
    updateSchedulePreview();
  });
});

fieldCron.addEventListener("input", updateSchedulePreview);
[fieldName, fieldDescription, fieldPayload].forEach((field) => {
  field.addEventListener("input", () => {
    setFormError("");
    syncFormValidity();
  });
});

function getCurrentSchedule() {
  if (currentMode === "simple") {
    return buildSimpleSchedule(
      parseInt(simpleMinute.value, 10),
      parseInt(simpleHour.value, 10),
      weekdaySelect.value,
    );
  }
  return fieldCron.value.trim();
}

function updateSchedulePreview() {
  const schedule = getCurrentSchedule();
  const previewEl = document.getElementById("schedule-preview");
  const valid = validateCron(schedule);

  if (!valid.ok) {
    previewEl.innerHTML = `<div class="preview-box error"><div class="preview-row"><span class="badge badge-failed">无效</span><span class="preview-label">${escapeHtml(valid.error)}</span></div></div>`;
    syncFormValidity();
    return;
  }

  const runs = nextRuns(schedule, 3);
  const human = humanizeCron(schedule);
  let html = `<div class="preview-box"><div class="preview-row"><span class="badge badge-running">有效</span>${human ? `<span class="preview-label">${escapeHtml(human)}</span>` : ""}</div>`;
  if (runs && runs.length > 0) {
    html += `<div class="preview-row"><span class="preview-label">下次触发：${formatLocal(runs[0])}</span></div>`;
    if (runs.length > 1) {
      const rest = runs.slice(1).map((d, i) => (i > 0 ? "、" : "") + relativeFromNow(d, runs[0])).join("");
      html += `<div class="preview-row"><span class="preview-label">接下来：${rest}</span></div>`;
    }
  }
  html += `</div>`;
  previewEl.innerHTML = html;
  syncFormValidity();
}

function setFormError(text) {
  formError.textContent = text;
  formError.hidden = !text;
}

function syncFormValidity() {
  const valid = validateCron(getCurrentSchedule()).ok;
  formSubmit.disabled = formSaving
    || !fieldName.value.trim()
    || !fieldDescription.value.trim()
    || !fieldPayload.value.trim()
    || !valid;
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

document.getElementById("create-btn").addEventListener("click", () => openCreateForm());
refreshBtn.addEventListener("click", () => loadJobs(true));
document.getElementById("form-close").addEventListener("click", closeForm);
document.getElementById("form-cancel").addEventListener("click", closeForm);

function loadSessions(selectedId) {
  fieldSession.innerHTML = "";
  const option = document.createElement("option");
  if (selectedId) {
    option.value = selectedId;
    option.textContent = selectedId;
  } else {
    option.value = "";
    option.textContent = "不关联，自动创建新会话";
  }
  fieldSession.appendChild(option);
  fieldSession.value = option.value;
}

function openCreateForm() {
  formTitle.textContent = "创建定时任务";
  formSubmit.textContent = "创建";
  fieldId.value = "";
  fieldName.value = "";
  fieldDescription.value = "";
  fieldPayload.value = "";
  setFormError("");
  // 默认简单模式
  currentMode = "simple";
  document.querySelector(".tab-simple").click();
  simpleMinute.value = "0";
  simpleHour.value = "9";
  weekdaySelect.value = "*";
  fieldCron.value = "0 0 9 * * *";
  simplePreview.textContent = buildSimpleSchedule(0, 9, "*");
  loadSessions("");
  updateSchedulePreview();
  formModal.hidden = false;
  syncHostMask();
  fieldName.focus();
}

function openEditForm(job) {
  formTitle.textContent = "编辑定时任务";
  formSubmit.textContent = "更新";
  fieldId.value = job.id;
  fieldName.value = job.name || "";
  fieldDescription.value = job.description || "";
  fieldPayload.value = job.payload || "";
  fieldCron.value = job.schedule || "0 0 9 * * *";
  setFormError("");

  // 尝试回填到简单模式
  const simple = tryParseToSimple(job.schedule);
  if (simple) {
    currentMode = "simple";
    document.querySelector(".tab-simple").click();
    simpleMinute.value = simple.minute;
    simpleHour.value = simple.hour;
    weekdaySelect.value = simple.weekday;
    simplePreview.textContent = buildSimpleSchedule(simple.minute, simple.hour, simple.weekday);
  } else {
    currentMode = "cron";
    document.querySelector(".tab-cron").click();
  }

  // 会话
  loadSessions(job.session_id || "");

  updateSchedulePreview();
  formModal.hidden = false;
  syncHostMask();
  fieldName.focus();
}

function closeForm() {
  formModal.hidden = true;
  setFormError("");
  syncHostMask();
}

async function submitJobForm() {
  const schedule = getCurrentSchedule();
  const valid = validateCron(schedule);
  if (!valid.ok) {
    setFormError(valid.error || "Cron 表达式无效");
    return;
  }
  const name = fieldName.value.trim();
  const description = fieldDescription.value.trim();
  const payload = fieldPayload.value.trim();
  if (!name || !description || !payload) {
    setFormError("请填写所有必填字段");
    return;
  }
  const sessionId = fieldSession.value || null;
  setFormError("");
  formSaving = true;
  syncFormValidity();
  formSubmit.textContent = "保存中…";
  try {
    const id = fieldId.value;
    let successMessage = "已创建";
    if (id) {
      await callHost("update", JSON.stringify({
        id,
        name,
        description,
        schedule,
        session_id: sessionId,
        payload,
      }));
      successMessage = "已更新";
    } else {
      await callHost("create", JSON.stringify({
        name,
        description,
        schedule,
        session_id: sessionId,
        payload,
        enabled: true,
      }));
    }
    closeForm();
    setStatus(successMessage);
    await loadJobs();
  } catch (err) {
    setFormError(`保存失败：${err.message || err}`);
  } finally {
    formSaving = false;
    formSubmit.textContent = fieldId.value ? "更新" : "创建";
    syncFormValidity();
  }
}

formSubmit.addEventListener("click", submitJobForm);
jobForm.addEventListener("keydown", (event) => {
  if (event.key !== "Enter" || event.target.tagName === "TEXTAREA" || formSubmit.disabled) return;
  event.preventDefault();
  submitJobForm();
});

// ── 启用/停用 ──

async function toggleJob(job, enabled, checkbox) {
  checkbox.disabled = true;
  try {
    await callHost("update", JSON.stringify({ id: job.id, enabled }));
    setStatus(enabled ? "已启用" : "已停用");
    await loadJobs();
  } catch (e) {
    setStatus(`操作失败：${e.message || e}`, true);
    await loadJobs();
  } finally {
    checkbox.disabled = false;
  }
}

// ── 删除 ──

async function deleteJob(job, button) {
  button.disabled = true;
  setStatus("删除中…");
  try {
    await callHost("delete", JSON.stringify({ id: job.id }));
    setStatus("已删除");
    await loadJobs();
  } catch (e) {
    setStatus(`删除失败：${e.message || e}`, true);
  } finally {
    button.disabled = false;
  }
}

// ── 触发 ──

async function triggerJob(job, button) {
  button.disabled = true;
  setStatus(`正在触发「${job.name || job.id}」…`);
  try {
    await callHost("trigger", JSON.stringify({ id: job.id }));
    setStatus(`已触发「${job.name || job.id}」，执行中`);
    setTimeout(loadJobs, 2000);
  } catch (e) {
    setStatus(`触发失败：${e.message || e}`, true);
  } finally {
    button.disabled = false;
  }
}

// ── 执行历史 ──

const runsModal = document.getElementById("runs-modal");
const runsTable = document.getElementById("runs-table");
const runsTbody = document.getElementById("runs-tbody");
const runsLoading = document.getElementById("runs-loading");
const runsEmpty = document.getElementById("runs-empty");

document.getElementById("runs-close").addEventListener("click", () => {
  runsModal.hidden = true;
  syncHostMask();
});

const STATUS_LABELS = { succeeded: "成功", failed: "失败", running: "运行中" };

function statusBadge(status) {
  const label = STATUS_LABELS[status] || status;
  const cls = status === "succeeded" ? "badge-success" : status === "failed" ? "badge-failed" : status === "running" ? "badge-running" : "badge-neutral";
  return `<span class="badge ${cls}">${escapeHtml(label)}</span>`;
}

async function showRuns(jobId) {
  runsModal.hidden = false;
  syncHostMask();
  runsTable.hidden = true;
  runsEmpty.hidden = true;
  runsEmpty.textContent = "暂无执行记录";
  runsLoading.hidden = false;
  try {
    const raw = await callHost("runs", JSON.stringify({ id: jobId, limit: 50 }));
    const data = raw ? JSON.parse(raw) : { runs: [] };
    const runs = data.runs || [];
    runsLoading.hidden = true;
    if (runs.length === 0) {
      runsEmpty.hidden = false;
      return;
    }
    runsTbody.innerHTML = "";
    for (const run of runs) {
      const tr = document.createElement("tr");
      const summary = run.result_summary || "-";
      tr.innerHTML = `<td>${statusBadge(run.status)}</td><td class="nowrap">${escapeHtml(run.started_at || "-")}</td><td class="nowrap">${escapeHtml(run.finished_at || "-")}</td><td title="${escapeHtml(summary)}">${escapeHtml(summary)}</td>`;
      runsTbody.appendChild(tr);
    }
    runsTable.hidden = false;
  } catch (e) {
    runsLoading.hidden = true;
    runsEmpty.textContent = `加载历史失败：${e.message || e}`;
    runsEmpty.hidden = false;
  }
}

// 挂载即加载列表。
loadJobs();
