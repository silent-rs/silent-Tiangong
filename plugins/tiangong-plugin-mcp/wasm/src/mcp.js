(() => {
  'use strict';

  const HOST_TIMEOUT_MS = 60000;
  const state = {
    servers: [],
    health: new Map(),
    editingName: null,
    removingName: null,
    loading: true,
  };

  let requestSequence = 0;
  let hostChannel = null;
  let hostReadyResolve;
  let lastModalFocus = null;
  let lastRemoveFocus = null;
  const hostReady = new Promise((resolve) => {
    hostReadyResolve = resolve;
  });

  const byId = (id) => document.getElementById(id);

  function escapeHtml(value) {
    return String(value ?? '')
      .replaceAll('&', '&amp;')
      .replaceAll('<', '&lt;')
      .replaceAll('>', '&gt;')
      .replaceAll('"', '&quot;')
      .replaceAll("'", '&#039;');
  }

  function errorText(error) {
    return error instanceof Error ? error.message : String(error);
  }

  function applyHostContext(context) {
    if (hostChannel && context.channel !== hostChannel) return;
    hostChannel = context.channel;
    const root = document.documentElement;
    root.dataset.theme = context.theme === 'dark' ? 'dark' : 'light';
    Object.entries(context.tokens || {}).forEach(([name, value]) => {
      if (typeof value === 'string' && value) {
        root.style.setProperty(`--host-${name}`, value);
      }
    });
    if (typeof context.fontFamily === 'string' && context.fontFamily) {
      root.style.setProperty('--host-font-family', context.fontFamily);
    }
    hostReadyResolve?.();
    hostReadyResolve = null;
  }

  window.addEventListener('message', (event) => {
    if (event.source !== window.parent || !event.data) return;
    if (event.data.type === 'tiangong_host_context' && typeof event.data.channel === 'string') {
      applyHostContext(event.data);
    }
  });
  window.parent.postMessage({ type: 'plugin_host_ready' }, '*');

  async function callHost(method, payload = {}) {
    if (!hostChannel) await hostReady;
    return new Promise((resolve, reject) => {
      const id = `mcp-${Date.now()}-${++requestSequence}`;
      const channel = hostChannel;
      const timeout = window.setTimeout(() => {
        window.removeEventListener('message', handler);
        reject(new Error('插件请求超时'));
      }, HOST_TIMEOUT_MS);
      const handler = (event) => {
        if (
          event.source !== window.parent
          || !event.data
          || event.data.id !== id
          || event.data.channel !== channel
        ) return;
        window.clearTimeout(timeout);
        window.removeEventListener('message', handler);
        if (event.data.error) {
          reject(new Error(String(event.data.error)));
        } else {
          resolve(event.data.result ?? '');
        }
      };
      window.addEventListener('message', handler);
      window.parent.postMessage({
        type: 'plugin_call',
        channel,
        id,
        method,
        payload: JSON.stringify(payload),
      }, '*');
    });
  }

  async function callJson(method, payload = {}) {
    const raw = await callHost(method, payload);
    return raw ? JSON.parse(raw) : {};
  }

  function showToast(message, type = 'success') {
    const toast = document.createElement('div');
    toast.className = `toast ${type === 'error' ? 'error' : ''}`;
    toast.textContent = message;
    byId('toast-region').appendChild(toast);
    window.setTimeout(() => toast.remove(), 3600);
  }

  function setRuntimeStatus(text, isError = false) {
    const target = byId('runtime-status');
    target.textContent = text;
    target.classList.toggle('error', isError);
  }

  function setHostMask(visible, color) {
    if (!hostChannel) return;
    window.parent.postMessage({
      type: 'plugin_host_mask',
      channel: hostChannel,
      visible,
      ...(color ? { color } : {}),
    }, '*');
  }

  function syncHostMask() {
    const modal = [byId('server-modal'), byId('remove-modal')]
      .find((element) => !element.classList.contains('hidden'));
    if (!modal) {
      setHostMask(false);
      return;
    }
    setHostMask(true, window.getComputedStyle(modal).backgroundColor);
  }

  function setLoading(loading) {
    state.loading = loading;
    byId('loading-state').classList.toggle('hidden', !loading);
    byId('server-list').classList.toggle('hidden', loading);
    byId('empty-state').classList.add('hidden');
    byId('refresh-button').disabled = loading;
  }

  function resolvedTransport(server) {
    if (server.transport === 'http') return 'http';
    if (server.transport === 'stdio') return 'stdio';
    const endpoint = String(server.endpoint || server.command || '').trim().toLowerCase();
    return endpoint.startsWith('http://') || endpoint.startsWith('https://') ? 'http' : 'stdio';
  }

  function serverTarget(server) {
    if (resolvedTransport(server) === 'http') {
      return server.endpoint || server.command || '未配置 Endpoint';
    }
    return [server.command, ...(server.args || [])].filter(Boolean).join(' ') || '未配置命令';
  }

  function refreshStats() {
    const enabled = state.servers.filter((server) => server.enabled).length;
    const healthy = state.servers.filter((server) => {
      const health = state.health.get(server.name);
      return server.enabled && health?.healthy;
    }).length;
    const tools = state.servers.reduce((total, server) => {
      if (!server.enabled) return total;
      return total + Number(state.health.get(server.name)?.tool_count || 0);
    }, 0);
    byId('stat-total').textContent = String(state.servers.length);
    byId('stat-enabled').textContent = String(enabled);
    byId('stat-healthy').textContent = String(healthy);
    byId('stat-tools').textContent = String(tools);
  }

  function refreshIcon() {
    return '<svg aria-hidden="true" viewBox="0 0 24 24"><path d="M20 12a8 8 0 1 1-2.34-5.66L20 8"/><path d="M20 4v4h-4"/></svg>';
  }

  function editIcon() {
    return '<svg aria-hidden="true" viewBox="0 0 24 24"><path d="M12 20h9"/><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z"/></svg>';
  }

  function trashIcon() {
    return '<svg aria-hidden="true" viewBox="0 0 24 24"><path d="M3 6h18M8 6V4h8v2M19 6l-1 14H6L5 6M10 11v5M14 11v5"/></svg>';
  }

  function healthMarkup(server) {
    if (!server.enabled) {
      return '<span class="status-dot disabled"></span><div><strong>已停用</strong><small>不会提供工具</small></div>';
    }
    const health = state.health.get(server.name);
    if (!health) {
      return '<span class="status-dot"></span><div><strong>等待探测</strong><small>尚无连接结果</small></div>';
    }
    if (health.healthy) {
      const version = health.server_version ? ` · v${escapeHtml(health.server_version)}` : '';
      return `<span class="status-dot healthy"></span><div><strong>连接正常</strong><small>${Number(health.tool_count || 0)} 个工具${version}</small></div>`;
    }
    return `<span class="status-dot error"></span><div><strong>连接失败</strong><small title="${escapeHtml(health.last_error || '')}">${escapeHtml(health.last_error || '无法连接服务器')}</small></div>`;
  }

  function renderServers() {
    refreshStats();
    const list = byId('server-list');
    const empty = byId('empty-state');
    if (state.servers.length === 0) {
      list.innerHTML = '';
      empty.classList.remove('hidden');
      return;
    }
    empty.classList.add('hidden');
    list.innerHTML = state.servers.map((server) => {
      const name = escapeHtml(server.name);
      const transport = resolvedTransport(server);
      const authCount = Object.keys(server.headers || {}).length;
      const detail = transport === 'http'
        ? [server.auth_header ? '已设置认证' : '', authCount ? `${authCount} 个 Header` : ''].filter(Boolean).join(' · ')
        : `${(server.env && Object.keys(server.env).length) || 0} 个环境变量`;
      return `
        <article class="server-row ${server.enabled ? '' : 'disabled'}">
          <div class="server-name">
            <strong>${name}</strong>
            <div class="badges">
              <span class="badge">${transport === 'http' ? 'HTTP / SSE' : 'stdio'}</span>
              <span class="badge">${server.enabled ? '已启用' : '已停用'}</span>
            </div>
          </div>
          <div class="server-target">
            <strong title="${escapeHtml(serverTarget(server))}">${escapeHtml(serverTarget(server))}</strong>
            <small>${escapeHtml(detail)}</small>
          </div>
          <div class="health-copy">${healthMarkup(server)}</div>
          <div class="row-actions">
            ${server.enabled ? `<button class="icon-button" type="button" data-action="probe" data-name="${name}" title="重新探测" aria-label="重新探测 ${name}">${refreshIcon()}</button>` : ''}
            <button class="icon-button" type="button" data-action="edit" data-name="${name}" title="编辑" aria-label="编辑 ${name}">${editIcon()}</button>
            <label class="switch" title="${server.enabled ? '停用' : '启用'} ${name}">
              <input type="checkbox" data-action="toggle" data-name="${name}" ${server.enabled ? 'checked' : ''}>
              <span></span>
            </label>
            <button class="icon-button danger" type="button" data-action="remove" data-name="${name}" title="删除" aria-label="删除 ${name}">${trashIcon()}</button>
          </div>
        </article>`;
    }).join('');

    list.querySelectorAll('button[data-action]').forEach((button) => {
      button.addEventListener('click', () => handleRowAction(button.dataset.action, button.dataset.name));
    });
    list.querySelectorAll('input[data-action="toggle"]').forEach((input) => {
      input.addEventListener('change', () => toggleServer(input.dataset.name, input.checked, input));
    });
  }

  async function loadData(options = {}) {
    const showLoading = options.showLoading !== false;
    if (showLoading) setLoading(true);
    try {
      const [listResponse, healthResponse] = await Promise.all([
        callJson('server.list'),
        callJson('server.health'),
      ]);
      state.servers = Array.isArray(listResponse.servers) ? listResponse.servers : [];
      state.health = new Map(
        (Array.isArray(healthResponse.statuses) ? healthResponse.statuses : [])
          .map((health) => [health.name, health]),
      );
      setRuntimeStatus('已连接');
      renderServers();
    } catch (error) {
      setRuntimeStatus('连接失败', true);
      showToast(errorText(error), 'error');
      if (state.servers.length === 0) byId('empty-state').classList.remove('hidden');
    } finally {
      setLoading(false);
    }
  }

  async function handleRefresh() {
    byId('refresh-button').disabled = true;
    setRuntimeStatus('正在刷新');
    try {
      const enabledServers = state.servers.filter((server) => server.enabled);
      await Promise.allSettled(
        enabledServers.map((server) => callHost('server.probe', { name: server.name })),
      );
      await loadData({ showLoading: false });
      showToast('MCP 状态已刷新');
    } finally {
      byId('refresh-button').disabled = false;
    }
  }

  async function handleRowAction(action, name) {
    if (!name) return;
    const server = state.servers.find((item) => item.name === name);
    if (!server) return;
    if (action === 'edit') {
      openModal(server);
      return;
    }
    if (action === 'remove') {
      openRemoveModal(server);
      return;
    }
    if (action === 'probe') {
      await probeServer(server);
    }
  }

  async function probeServer(server) {
    setRuntimeStatus(`正在探测 ${server.name}`);
    try {
      await callHost('server.probe', { name: server.name });
      await loadData({ showLoading: false });
    } catch (error) {
      setRuntimeStatus('探测失败', true);
      showToast(errorText(error), 'error');
      await loadData({ showLoading: false });
    }
  }

  async function toggleServer(name, enabled, input) {
    input.disabled = true;
    try {
      const response = await callJson('server.set_enabled', { name, enabled });
      showToast(response.message || `服务器已${enabled ? '启用' : '停用'}`);
      await loadData({ showLoading: false });
    } catch (error) {
      input.checked = !enabled;
      showToast(errorText(error), 'error');
    } finally {
      input.disabled = false;
    }
  }

  function openRemoveModal(server) {
    state.removingName = server.name;
    byId('remove-message').textContent = `确定删除 MCP 服务器“${server.name}”吗？`;
    lastRemoveFocus = document.activeElement;
    byId('remove-modal').classList.remove('hidden');
    syncHostMask();
    window.setTimeout(() => byId('remove-cancel').focus(), 0);
  }

  function closeRemoveModal() {
    byId('remove-modal').classList.add('hidden');
    syncHostMask();
    state.removingName = null;
    lastRemoveFocus?.focus?.();
    lastRemoveFocus = null;
  }

  async function confirmRemoveServer() {
    const name = state.removingName;
    if (!name) return;
    const confirmButton = byId('remove-confirm');
    confirmButton.disabled = true;
    try {
      const response = await callJson('server.remove', { name });
      closeRemoveModal();
      showToast(response.message || '服务器已删除');
      await loadData({ showLoading: false });
    } catch (error) {
      showToast(errorText(error), 'error');
    } finally {
      confirmButton.disabled = false;
    }
  }

  function formatKeyValue(values) {
    return Object.entries(values || {})
      .map(([key, value]) => `${key}=${value}`)
      .join('\n');
  }

  function parseKeyValue(value, label) {
    const entries = [];
    for (const rawLine of value.split(/[,\n]/)) {
      const line = rawLine.trim();
      if (!line) continue;
      const separator = line.includes('=') ? line.indexOf('=') : line.indexOf(':');
      if (separator <= 0) throw new Error(`${label}格式错误：${line}`);
      const key = line.slice(0, separator).trim();
      const itemValue = line.slice(separator + 1).trim();
      if (!key || !itemValue) throw new Error(`${label}格式错误：${line}`);
      entries.push([key, itemValue]);
    }
    return entries;
  }

  function setTransportFields() {
    const isStdio = byId('server-transport').value === 'stdio';
    byId('stdio-fields').classList.toggle('hidden', !isStdio);
    byId('http-fields').classList.toggle('hidden', isStdio);
  }

  function resetForm() {
    byId('server-form').reset();
    byId('server-transport').value = 'stdio';
    byId('server-name').disabled = false;
    byId('form-error').textContent = '';
    setTransportFields();
  }

  function openModal(server = null) {
    resetForm();
    state.editingName = server?.name || null;
    byId('modal-title').textContent = server ? `编辑 MCP 服务器：${server.name}` : '添加 MCP 服务器';
    if (server) {
      byId('server-name').value = server.name;
      byId('server-name').disabled = true;
      byId('server-transport').value = resolvedTransport(server);
      byId('server-command').value = server.command || '';
      byId('server-args').value = (server.args || []).join(' ');
      byId('server-env').value = formatKeyValue(server.env);
      byId('server-endpoint').value = server.endpoint || '';
      byId('server-auth-header').value = server.auth_header || '';
      byId('server-headers').value = formatKeyValue(server.headers);
      setTransportFields();
    }
    lastModalFocus = document.activeElement;
    byId('server-modal').classList.remove('hidden');
    syncHostMask();
    window.setTimeout(() => byId(server ? 'server-transport' : 'server-name').focus(), 0);
  }

  function closeModal() {
    byId('server-modal').classList.add('hidden');
    syncHostMask();
    state.editingName = null;
    byId('form-error').textContent = '';
    lastModalFocus?.focus?.();
    lastModalFocus = null;
  }

  function buildServerRequest() {
    const name = byId('server-name').value.trim();
    const transport = byId('server-transport').value;
    const isStdio = transport === 'stdio';
    const command = byId('server-command').value.trim();
    const endpoint = byId('server-endpoint').value.trim();
    if (!name) throw new Error('服务器名称不能为空');
    if (isStdio && !command) throw new Error('本地连接必须填写命令');
    if (!isStdio && !endpoint) throw new Error('远程连接必须填写 Endpoint');
    const existing = state.servers.find((server) => server.name === state.editingName);
    return {
      name,
      command: isStdio ? command : '',
      args: isStdio
        ? byId('server-args').value.split(/\s+/).map((item) => item.trim()).filter(Boolean)
        : [],
      tags: existing?.tags || [],
      enabled: existing?.enabled ?? true,
      options: {
        transport,
        endpoint: isStdio ? null : endpoint,
        auth_header: isStdio ? null : (byId('server-auth-header').value.trim() || null),
        headers: isStdio ? [] : parseKeyValue(byId('server-headers').value, 'Header'),
        env: isStdio ? parseKeyValue(byId('server-env').value, '环境变量') : [],
      },
    };
  }

  async function saveServer(event) {
    event.preventDefault();
    const saveButton = byId('save-button');
    const formError = byId('form-error');
    formError.textContent = '';
    saveButton.disabled = true;
    try {
      const request = buildServerRequest();
      const editingName = state.editingName;
      const response = editingName
        ? await callJson('server.update', { ...request, name: editingName })
        : await callJson('server.register', request);
      closeModal();
      showToast(response.message || `服务器已${editingName ? '更新' : '添加'}`);
      await loadData({ showLoading: false });
    } catch (error) {
      formError.textContent = errorText(error);
    } finally {
      saveButton.disabled = false;
    }
  }

  byId('refresh-button').addEventListener('click', handleRefresh);
  byId('add-button').addEventListener('click', () => openModal());
  byId('modal-close').addEventListener('click', closeModal);
  byId('cancel-button').addEventListener('click', closeModal);
  byId('remove-modal-close').addEventListener('click', closeRemoveModal);
  byId('remove-cancel').addEventListener('click', closeRemoveModal);
  byId('remove-confirm').addEventListener('click', confirmRemoveServer);
  byId('server-transport').addEventListener('change', setTransportFields);
  byId('server-form').addEventListener('submit', saveServer);
  byId('server-modal').addEventListener('click', (event) => {
    if (event.target === byId('server-modal')) closeModal();
  });
  byId('remove-modal').addEventListener('click', (event) => {
    if (event.target === byId('remove-modal')) closeRemoveModal();
  });
  document.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') return;
    if (!byId('server-modal').classList.contains('hidden')) {
      closeModal();
    } else if (!byId('remove-modal').classList.contains('hidden')) {
      closeRemoveModal();
    }
  });

  loadData();
})();
