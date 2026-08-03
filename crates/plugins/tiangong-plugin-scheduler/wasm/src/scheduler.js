// 定时任务设置页脚本。
//
// 与 index.js / memory.js 同构的 host context + callHost 桥接框架。

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
  // 简单语法校验：每个字段只允许数字、*、-、,、/
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
  let cursor = new Date(now.getTime() + 1000);
  cursor.setMilliseconds(0);
  let iterations = 0;
  const maxIterations = 500000;

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
    cursor = new Date(cursor.getTime() + 1000);
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
  // */n 步进
  const stepMatch = part.match(/^\*\/(\d+)$/);
  if (stepMatch) {
    const step = parseInt(stepMatch[1], 10);
    return value % step === 0;
  }
  // a-b 范围
  const rangeMatch = part.match(/^(\d+)-(\d+)$/);
  if (rangeMatch) {
    const lo = parseInt(rangeMatch[1], 10);
    const hi = parseInt(rangeMatch[2], 10);
    return value >= lo && value <= hi;
  }
  // a/b 步进
  const slashMatch = part.match(/^(\d+)\/(\d+)$/);
  if (slashMatch) {
    const base = parseInt(slashMatch[1], 10);
    const step = parseInt(slashMatch[2], 10);
    return value >= base && (value - base) % step === 0;
  }
  // 单个数字
  const num = parseInt(part, 10);
  return num === value;
}

/** 周字段匹配：cron 中 0/7=周日，JS getDay() 中 0=周日。 */
function matchDow(dowField, jsDay) {
  if (dowField === "*") return true;
  // 把 7 当作 0（周日）
  const normalize = (v) => (v === 7 ? 0 : v);
  for (const part of dowField.split(",")) {
    if (part.includes("-")) {
      const [lo, hi] = part.split("-").map((s) => normalize(parseInt(s, 10)));
      if (lo <= hi) {
        if (jsDay >= lo && jsDay <= hi) return true;
      } else {
        // 跨周边界，如 5-0（周五到周日）
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

// ── DOM 引用 ──

const listEl = document.getElementById("list");
const statusEl = document.getElementById("status");
const emptyTemplate = document.getElementById("empty-template");
const rowTemplate = document.getElementById("row-template");

function setStatus(text, isError = false) {
  statusEl.textContent = text;
  statusEl.classList.toggle("error", isError);
}

// ── 任务列表 ──

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
    node.querySelector(".job-row-desc").textContent = job.description || "";
    node.dataset.jobId = job.id;

    const checkbox = node.querySelector(".toggle-checkbox");
    checkbox.checked = job.enabled !== false;
    checkbox.addEventListener("change", () => toggleJob(job, checkbox.checked));

    node.querySelector(".trigger-btn").addEventListener("click", () => triggerJob(job));
    node.querySelector(".runs-btn").addEventListener("click", () => showRuns(job.id));
    node.querySelector(".edit-btn").addEventListener("click", () => openEditForm(job));
    node.querySelector(".delete-btn").addEventListener("click", () => deleteJob(job));

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
    previewEl.innerHTML = `<div class="preview-box error"><span class="badge badge-failed">无效</span><span class="preview-label">${valid.error}</span></div>`;
    return;
  }

  const runs = nextRuns(schedule, 3);
  let html = `<div class="preview-box"><div class="preview-row"><span class="badge badge-success">有效</span></div>`;
  if (runs && runs.length > 0) {
    html += `<div class="preview-row"><span class="preview-label">下次触发：${formatLocal(runs[0])}</span></div>`;
    if (runs.length > 1) {
      const rest = runs.slice(1).map((d, i) => (i > 0 ? "、" : "") + relativeFromNow(d, runs[0])).join("");
      html += `<div class="preview-row"><span class="preview-label">接下来：${rest}</span></div>`;
    }
  }
  html += `</div>`;
  previewEl.innerHTML = html;
}

document.getElementById("create-btn").addEventListener("click", () => openCreateForm());
document.getElementById("form-close").addEventListener("click", closeForm);
document.getElementById("form-cancel").addEventListener("click", closeForm);

async function loadSessions(selectedId) {
  // 清空除第一个以外的选项
  while (fieldSession.children.length > 1) {
    fieldSession.removeChild(fieldSession.lastChild);
  }
  fieldSession.value = "";
  // 不阻塞表单打开——会话列表加载失败时静默降级为手动输入
}

function openCreateForm() {
  formTitle.textContent = "创建定时任务";
  formSubmit.textContent = "创建";
  fieldId.value = "";
  fieldName.value = "";
  fieldDescription.value = "";
  fieldPayload.value = "";
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
}

function openEditForm(job) {
  formTitle.textContent = "编辑定时任务";
  formSubmit.textContent = "更新";
  fieldId.value = job.id;
  fieldName.value = job.name || "";
  fieldDescription.value = job.description || "";
  fieldPayload.value = job.payload || "";

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
    fieldCron.value = job.schedule || "0 0 9 * * *";
  }

  // 会话
  loadSessions(job.session_id || "");

  updateSchedulePreview();
  formModal.hidden = false;
}

function closeForm() {
  formModal.hidden = true;
}

jobForm.addEventListener("submit", async (e) => {
  e.preventDefault();
  const schedule = getCurrentSchedule();
  const valid = validateCron(schedule);
  if (!valid.ok) {
    setStatus(valid.error || "Cron 表达式无效", true);
    return;
  }
  const name = fieldName.value.trim();
  const description = fieldDescription.value.trim();
  const payload = fieldPayload.value.trim();
  if (!name || !description || !payload) {
    setStatus("请填写所有必填字段", true);
    return;
  }
  const sessionId = fieldSession.value || null;
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
        session_id: sessionId,
        payload,
      }));
      setStatus("已更新");
    } else {
      await callHost("create", JSON.stringify({
        name,
        description,
        schedule,
        session_id: sessionId,
        payload,
        enabled: true,
      }));
      setStatus("已创建");
    }
    closeForm();
    await loadJobs();
  } catch (err) {
    setStatus(`保存失败：${err.message || err}`, true);
  } finally {
    formSubmit.disabled = false;
    formSubmit.textContent = fieldId.value ? "更新" : "创建";
  }
});

// ── 启用/停用 ──

async function toggleJob(job, enabled) {
  try {
    await callHost("update", JSON.stringify({ id: job.id, enabled }));
    setStatus(enabled ? "已启用" : "已停用");
  } catch (e) {
    setStatus(`操作失败：${e.message || e}`, true);
    await loadJobs();
  }
}

// ── 删除 ──

async function deleteJob(job) {
  if (!confirm(`确认删除任务「${job.name || job.id}」？`)) return;
  setStatus("删除中…");
  try {
    await callHost("delete", JSON.stringify({ id: job.id }));
    setStatus("已删除");
    await loadJobs();
  } catch (e) {
    setStatus(`删除失败：${e.message || e}`, true);
  }
}

// ── 触发 ──

async function triggerJob(job) {
  setStatus(`正在触发「${job.name || job.id}」…`);
  try {
    await callHost("trigger", JSON.stringify({ id: job.id }));
    setStatus(`已触发「${job.name || job.id}」，执行中`);
  } catch (e) {
    setStatus(`触发失败：${e.message || e}`, true);
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
});

const STATUS_LABELS = { succeeded: "成功", failed: "失败", running: "运行中" };

function statusBadge(status) {
  const label = STATUS_LABELS[status] || status;
  const cls = status === "succeeded" ? "badge-success" : status === "failed" ? "badge-failed" : status === "running" ? "badge-running" : "";
  return `<span class="badge ${cls}">${label}</span>`;
}

async function showRuns(jobId) {
  runsModal.hidden = false;
  runsTable.hidden = true;
  runsEmpty.hidden = true;
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
      tr.innerHTML = `<td>${statusBadge(run.status)}</td><td class="nowrap">${run.started_at || "-"}</td><td class="nowrap">${run.finished_at || "-"}</td><td class="truncate" title="${(run.result_summary || "").replace(/"/g, "&quot;")}">${run.result_summary || "-"}</td>`;
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
