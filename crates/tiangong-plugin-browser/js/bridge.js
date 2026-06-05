(function() {
    if (window.__tiangong_bridge_loaded) return;
    window.__tiangong_bridge_loaded = true;

    // 屏蔽 Tauri IPC — 浏览器 WebView 加载外部 URL，不应暴露 Tauri API
    //
    // 策略：
    // 1. 用 setter 陷阱拦截 __TAURI_INTERNALS__（Tauri 后续注入时会触发我们的 setter）
    // 2. 让所有 IPC 调用静默返回成功空结果，避免页面 JS 报错
    // 3. 拦截 fetch/XHR 屏蔽 ipc:// 协议
    try {
        // --- 1. 陷阱 __TAURI_INTERNALS__ ---
        // 我们的 initialization_script 在 Tauri 的 runtime 脚本之前运行，
        // 所以可以抢先定义 __TAURI_INTERNALS__ 为带 setter 陷阱的属性。
        // 当 Tauri 后续注入真实的 __TAURI_INTERNALS__ 时，setter 触发，
        // 我们保留一个 noop 版本。
        var _noopInternals = {
            invoke: function() { return Promise.resolve('{}'); },
            ipc: { postMessage: function() {} },
            postMessage: function() {},
            convertCallback: function(cb) {
                var id = Math.floor(Math.random() * 1e9);
                return cb ? id : id;
            },
            transformCallback: function(cb, once) {
                var id = Math.floor(Math.random() * 1e9);
                return cb ? id : id;
            },
            metadata: {
                currentWebview: { label: 'browser-webview' },
                currentWindow: { label: 'main' }
            }
        };
        try {
            Object.defineProperty(window, '__TAURI_INTERNALS__', {
                configurable: true,
                enumerable: false,
                get: function() { return _noopInternals; },
                set: function(val) {
                    // Tauri 尝试设置真实 internals 时，复制 metadata 但保持 noop
                    if (val && val.metadata) {
                        _noopInternals.metadata = val.metadata;
                    }
                }
            });
        } catch(e) {
            // __TAURI_INTERNALS__ 已被 Tauri 定义（不可配置），直接覆盖方法
            if (window.__TAURI_INTERNALS__) {
                try { window.__TAURI_INTERNALS__.invoke = function() { return Promise.resolve('{}'); }; } catch(e2) {}
                try { window.__TAURI_INTERNALS__.ipc = { postMessage: function() {} }; } catch(e2) {}
                try { window.__TAURI_INTERNALS__.postMessage = function() {}; } catch(e2) {}
            }
        }

        // --- 2. 屏蔽 window.ipc ---
        // window.ipc 由 WRY 在我们的脚本之前注入，使用 Object.defineProperty + Object.freeze，
        // 不可覆盖。但我们可以在其 postMessage 被调用前拦截。
        try {
            Object.defineProperty(window, 'ipc', {
                configurable: true,
                enumerable: false,
                value: Object.freeze({ postMessage: function() {} })
            });
        } catch(e) {
            // window.ipc 不可重新配置，忽略
        }

        // --- 3. 拦截 fetch ipc:// 协议 ---
        // 返回包含 Tauri 期望头部的成功响应，避免回退到 postMessage 通道
        var _origFetch = window.fetch;
        window.fetch = function(input, init) {
            var url = (typeof input === 'string') ? input : (input && input.url ? input.url : '');
            if (url.indexOf('ipc://') === 0) {
                return Promise.resolve(new Response('{}', {
                    status: 200,
                    headers: {
                        'Content-Type': 'application/json',
                        'Tauri-Response': 'ok',
                        'Access-Control-Allow-Origin': '*',
                        'Access-Control-Expose-Headers': 'Tauri-Response'
                    }
                }));
            }
            return _origFetch.apply(this, arguments);
        };

        // --- 4. 拦截 XHR ipc:// 协议 ---
        var _origXHR = window.XMLHttpRequest.prototype.open;
        window.XMLHttpRequest.prototype.open = function(method, url) {
            if (typeof url === 'string' && url.indexOf('ipc://') === 0) {
                return;
            }
            return _origXHR.apply(this, arguments);
        };
    } catch(e) {}

    window.__tiangong_bridge = {
        version: '0.8.0',

        detectFramework: function() {
            var r = { frameworks: [], uiLibraries: [] };
            // React
            if (document.querySelector('[data-reactroot]') ||
                document.querySelector('[data-reactid]') ||
                window.__REACT_DEVTOOLS_GLOBAL_HOOK__) {
                r.frameworks.push('react');
            }
            // Vue 2/3
            if (document.querySelector('[data-server-rendered]') ||
                document.querySelector('[data-v-]') ||
                window.__VUE__) {
                r.frameworks.push('vue');
            }
            // Vue 3 组件带 __vue_app__
            if (document.querySelector('[data-v-inspector]') ||
                (document.querySelector && Array.from(document.querySelectorAll('*')).some(function(el) {
                    return el.__vue_app__;
                }))) {
                if (r.frameworks.indexOf('vue') === -1) r.frameworks.push('vue');
            }
            // Ant Design
            if (document.querySelector('.ant-select') || document.querySelector('.ant-picker') ||
                document.querySelector('.ant-form-item') || document.querySelector('.ant-btn')) {
                r.uiLibraries.push('antd');
            }
            // Element Plus / Element UI
            if (document.querySelector('.el-select') || document.querySelector('.el-date-editor') ||
                document.querySelector('.el-form-item') || document.querySelector('.el-button')) {
                r.uiLibraries.push('element-plus');
            }
            return r;
        },

        getFullText: function(maxChars) {
            maxChars = maxChars || 12000;
            var text = '';
            if (document.body) {
                var clone = document.body.cloneNode(true);
                var removes = clone.querySelectorAll('script,style,noscript');
                for (var i = 0; i < removes.length; i++) {
                    removes[i].parentNode.removeChild(removes[i]);
                }
                text = (clone.textContent || '').replace(/\s+/g, ' ').trim();
                if (text.length < 50) {
                    text = (document.body.innerText || '').trim();
                }
            }
            if (text.length > maxChars) {
                text = text.substring(0, maxChars) + '\n...[内容已截断]';
            }
            return {
                title: document.title,
                url: window.location.href,
                text: text,
            };
        },

        click: function(selector) {
            var el = document.querySelector(selector);
            if (el) { el.click(); return true; }
            return false;
        },

        type: function(selector, text) {
            var el = document.querySelector(selector);
            if (!el) return false;
            el.focus();
            var nativeSetter = Object.getOwnPropertyDescriptor(
                HTMLInputElement.prototype, 'value'
            );
            if (nativeSetter && nativeSetter.set) {
                nativeSetter.set.call(el, text);
            } else {
                el.value = text;
            }
            el.dispatchEvent(new Event('input', { bubbles: true }));
            el.dispatchEvent(new Event('change', { bubbles: true }));
            return true;
        },

        extractForms: function() {
            var forms = [];
            var containers = document.querySelectorAll('form');
            if (containers.length === 0) {
                containers = [document.body];
            }
            for (var ci = 0; ci < containers.length; ci++) {
                var container = containers[ci];
                var fields = [];
                var inputs = container.querySelectorAll(
                    'input:not([type="hidden"]):not([type="submit"]):not([type="button"]):not([type="reset"]),textarea,select'
                );
                for (var idx = 0; idx < inputs.length; idx++) {
                    var el = inputs[idx];
                    var field = {
                        index: idx,
                        tag: el.tagName.toLowerCase(),
                        type: el.type || '',
                        name: el.name || '',
                        id: el.id || '',
                        label: '',
                        placeholder: el.placeholder || '',
                        value: el.value || '',
                        required: el.required || false,
                        readonly: el.readOnly || false,
                        disabled: el.disabled || false,
                        selector: ''
                    };
                    // 尝试关联 label
                    if (el.id) {
                        var lbl = container.querySelector('label[for="' + el.id + '"]');
                        if (lbl) field.label = (lbl.textContent || '').trim();
                    }
                    if (!field.label) {
                        var parent = el.parentElement;
                        if (parent && parent.tagName === 'LABEL') {
                            field.label = (parent.textContent || '').trim();
                        }
                    }
                    // select 的 options
                    if (el.tagName === 'SELECT') {
                        field.options = [];
                        var opts = el.querySelectorAll('option');
                        for (var oi = 0; oi < opts.length; oi++) {
                            field.options.push({
                                value: opts[oi].value,
                                text: (opts[oi].textContent || '').trim()
                            });
                        }
                    }
                    // 构造 selector
                    if (el.id) {
                        field.selector = '#' + el.id.replace(/([^\w-])/g, '\\$1');
                    } else if (el.name) {
                        field.selector = '[name="' + el.name + '"]';
                    } else {
                        field.selector = el.tagName.toLowerCase() + ':nth-of-type(' + (idx + 1) + ')';
                    }
                    fields.push(field);
                }
                if (fields.length > 0) {
                    forms.push({ fields: fields });
                }
            }
            return { forms: forms, framework: this.detectFramework(), uiComponents: this._extractUIComponents() };
        },

        _extractUIComponents: function() {
            var components = [];
            // Ant Design Select
            var antSelects = document.querySelectorAll('.ant-select');
            for (var i = 0; i < antSelects.length; i++) {
                var sel = antSelects[i];
                var selItem = sel.querySelector('.ant-select-selection-item');
                var placeholder = sel.querySelector('.ant-select-selection-placeholder');
                var input = sel.querySelector('input');
                components.push({
                    componentType: 'antd-select',
                    selector: '.ant-select:nth-of-type(' + (i + 1) + ')',
                    label: this._getAntFormItemLabel(sel),
                    placeholder: placeholder ? (placeholder.textContent || '').trim() : '',
                    value: selItem ? (selItem.textContent || '').trim() : '',
                    name: input ? (input.name || '') : '',
                    id: sel.id || '',
                    disabled: sel.classList.contains('ant-select-disabled'),
                    required: false,
                    readonly: false,
                    tag: 'div',
                    type: 'antd-select',
                    index: components.length
                });
            }
            // Ant Design DatePicker
            var antPickers = document.querySelectorAll('.ant-picker');
            for (var j = 0; j < antPickers.length; j++) {
                var picker = antPickers[j];
                var pickerInput = picker.querySelector('input');
                components.push({
                    componentType: 'antd-datepicker',
                    selector: '.ant-picker:nth-of-type(' + (j + 1) + ')',
                    label: this._getAntFormItemLabel(picker),
                    placeholder: pickerInput ? (pickerInput.placeholder || '') : '',
                    value: pickerInput ? (pickerInput.value || '') : '',
                    name: pickerInput ? (pickerInput.name || '') : '',
                    id: picker.id || '',
                    disabled: picker.classList.contains('ant-picker-disabled'),
                    required: false,
                    readonly: false,
                    tag: 'div',
                    type: 'antd-datepicker',
                    index: components.length
                });
            }
            // Element Plus Select
            var elSelects = document.querySelectorAll('.el-select');
            for (var k = 0; k < elSelects.length; k++) {
                var es = elSelects[k];
                var esInput = es.querySelector('.el-input__inner') || es.querySelector('input');
                var esPlaceholder = esInput ? (esInput.placeholder || '') : '';
                var esSuffix = es.querySelector('.el-select__suffix');
                // 检查是否有已选值显示
                var esSelected = es.querySelector('.el-select__selected-item') ||
                                 es.querySelector('.el-input__inner');
                components.push({
                    componentType: 'el-select',
                    selector: '.el-select:nth-of-type(' + (k + 1) + ')',
                    label: this._getElFormItemLabel(es),
                    placeholder: esPlaceholder,
                    value: esSelected ? (esSelected.textContent || '').trim() : '',
                    name: esInput ? (esInput.name || '') : '',
                    id: es.id || '',
                    disabled: es.classList.contains('is-disabled'),
                    required: false,
                    readonly: false,
                    tag: 'div',
                    type: 'el-select',
                    index: components.length
                });
            }
            // Element Plus DatePicker
            var elDates = document.querySelectorAll('.el-date-editor');
            for (var m = 0; m < elDates.length; m++) {
                var ed = elDates[m];
                var edInput = ed.querySelector('input');
                components.push({
                    componentType: 'el-datepicker',
                    selector: '.el-date-editor:nth-of-type(' + (m + 1) + ')',
                    label: this._getElFormItemLabel(ed),
                    placeholder: edInput ? (edInput.placeholder || '') : '',
                    value: edInput ? (edInput.value || '') : '',
                    name: edInput ? (edInput.name || '') : '',
                    id: ed.id || '',
                    disabled: ed.classList.contains('is-disabled'),
                    required: false,
                    readonly: false,
                    tag: 'div',
                    type: 'el-datepicker',
                    index: components.length
                });
            }
            return components;
        },

        _getAntFormItemLabel: function(el) {
            var formItem = el.closest('.ant-form-item');
            if (formItem) {
                var lbl = formItem.querySelector('.ant-form-item-label');
                if (lbl) return (lbl.textContent || '').trim();
            }
            return '';
        },

        _getElFormItemLabel: function(el) {
            var formItem = el.closest('.el-form-item');
            if (formItem) {
                var lbl = formItem.querySelector('.el-form-item__label');
                if (lbl) return (lbl.textContent || '').trim();
            }
            return '';
        },

        fillField: function(selector, value, strategy) {
            var el = this.locateElement(selector);
            if (!el) return { ok: false, error: '元素未找到: ' + selector };

            // select 特殊处理
            if (el.tagName === 'SELECT') {
                el.value = value;
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return { ok: true, strategy: 'select-change' };
            }

            // checkbox / radio 特殊处理
            if (el.type === 'checkbox' || el.type === 'radio') {
                var shouldCheck = (value === 'true' || value === '1');
                if (el.checked !== shouldCheck) {
                    el.click();
                }
                return { ok: true, strategy: 'click-toggle' };
            }

            strategy = strategy || 'auto';

            // 策略 1: 逐字符键盘输入（视觉层最可靠，适用于所有框架）
            if (strategy === 'auto' || strategy === 'keyboard') {
                el.focus();
                el.dispatchEvent(new KeyboardEvent('keydown', { key: '', bubbles: true }));
                el.value = '';
                el.dispatchEvent(new Event('input', { bubbles: true }));
                for (var i = 0; i < value.length; i++) {
                    var ch = value[i];
                    el.value = el.value + ch;
                    var keyInit = { key: ch, code: 'Key' + ch.toUpperCase(), bubbles: true };
                    el.dispatchEvent(new KeyboardEvent('keydown', keyInit));
                    el.dispatchEvent(new KeyboardEvent('keypress', keyInit));
                    el.dispatchEvent(new Event('input', { bubbles: true }));
                    el.dispatchEvent(new KeyboardEvent('keyup', keyInit));
                }
                el.dispatchEvent(new Event('change', { bubbles: true }));
                if (el.value === value) {
                    return { ok: true, strategy: 'keyboard' };
                }
            }

            // 策略 2: native setter（适用于 React 受控组件，视觉层可能不跟随）
            if (strategy === 'auto' || strategy === 'native') {
                el.focus();
                var proto = Object.getPrototypeOf(el);
                var descriptor = Object.getOwnPropertyDescriptor(proto, 'value') ||
                                 Object.getOwnPropertyDescriptor(
                                     el.tagName === 'TEXTAREA' ?
                                         HTMLTextAreaElement.prototype :
                                         HTMLInputElement.prototype,
                                     'value'
                                 );
                if (descriptor && descriptor.set) {
                    el.value = '';
                    el.dispatchEvent(new Event('input', { bubbles: true }));
                    descriptor.set.call(el, value);
                    el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: value }));
                    el.dispatchEvent(new Event('change', { bubbles: true }));
                    if (el.value === value) {
                        return { ok: true, strategy: 'native-setter' };
                    }
                }
            }

            // 策略 3: 粘贴（兜底）
            if (strategy === 'auto' || strategy === 'paste') {
                el.focus();
                el.value = value;
                el.dispatchEvent(new Event('input', { bubbles: true }));
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return { ok: true, strategy: 'paste' };
            }

            return { ok: false, error: '所有填写策略均未成功', currentValue: el.value };
        },

        // UI 库组件填写（多步交互，异步返回）
        fillComponent: function(selector, value) {
            var el = document.querySelector(selector);
            if (!el) return { ok: false, error: '组件未找到: ' + selector };

            // Ant Design Select
            if (el.classList.contains('ant-select')) {
                return this._fillAntSelect(el, value);
            }
            // Ant Design DatePicker
            if (el.classList.contains('ant-picker')) {
                return this._fillAntDatePicker(el, value);
            }
            // Element Plus Select
            if (el.classList.contains('el-select')) {
                return this._fillElSelect(el, value);
            }
            // Element Plus DatePicker
            if (el.classList.contains('el-date-editor')) {
                return this._fillElDatePicker(el, value);
            }
            return { ok: false, error: '未知的 UI 库组件类型' };
        },

        _fillAntSelect: function(el, value) {
            // 点击打开下拉框
            var trigger = el.querySelector('.ant-select-selector');
            if (!trigger) return { ok: false, error: 'Ant Select 触发器未找到' };
            trigger.click();
            // 异步选择：用 setTimeout 等待下拉框渲染
            var self = this;
            setTimeout(function() {
                var dropdown = document.querySelector('.ant-select-dropdown:not(.ant-select-dropdown-hidden)');
                if (!dropdown) {
                    // 下拉框可能挂在 body 上
                    var allDropdowns = document.querySelectorAll('.ant-select-dropdown');
                    for (var d = 0; d < allDropdowns.length; d++) {
                        if (allDropdowns[d].offsetParent !== null) {
                            dropdown = allDropdowns[d];
                            break;
                        }
                    }
                }
                if (!dropdown) return;
                var items = dropdown.querySelectorAll('.ant-select-item-option');
                for (var i = 0; i < items.length; i++) {
                    var text = (items[i].textContent || '').trim();
                    if (text === value || text.indexOf(value) >= 0) {
                        items[i].click();
                        return;
                    }
                }
            }, 300);
            return { ok: true, strategy: 'antd-select', note: '异步执行中，稍后检查结果' };
        },

        _fillAntDatePicker: function(el, value) {
            var input = el.querySelector('input');
            if (!input) return { ok: false, error: 'DatePicker 输入框未找到' };
            input.focus();
            // 清空并输入
            var nativeSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');
            if (nativeSetter && nativeSetter.set) {
                nativeSetter.set.call(input, value);
            } else {
                input.value = value;
            }
            input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: value }));
            input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', keyCode: 13, bubbles: true }));
            input.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', code: 'Enter', keyCode: 13, bubbles: true }));
            return { ok: true, strategy: 'antd-datepicker' };
        },

        _fillElSelect: function(el, value) {
            var input = el.querySelector('.el-input') || el.querySelector('input');
            if (!input) return { ok: false, error: 'El Select 输入框未找到' };
            input.click();
            var self = this;
            setTimeout(function() {
                var dropdown = document.querySelector('.el-select-dropdown:not([style*="display: none"])');
                if (!dropdown) {
                    var allDropdowns = document.querySelectorAll('.el-select-dropdown');
                    for (var d = 0; d < allDropdowns.length; d++) {
                        if (allDropdowns[d].offsetParent !== null) {
                            dropdown = allDropdowns[d];
                            break;
                        }
                    }
                }
                if (!dropdown) return;
                var items = dropdown.querySelectorAll('.el-select-dropdown__item');
                for (var i = 0; i < items.length; i++) {
                    var text = (items[i].textContent || '').trim();
                    if (text === value || text.indexOf(value) >= 0) {
                        items[i].click();
                        return;
                    }
                }
            }, 300);
            return { ok: true, strategy: 'el-select', note: '异步执行中，稍后检查结果' };
        },

        _fillElDatePicker: function(el, value) {
            var input = el.querySelector('input');
            if (!input) return { ok: false, error: 'El DatePicker 输入框未找到' };
            input.focus();
            var nativeSetter = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');
            if (nativeSetter && nativeSetter.set) {
                nativeSetter.set.call(input, value);
            } else {
                input.value = value;
            }
            input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: value }));
            input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', code: 'Enter', keyCode: 13, bubbles: true }));
            input.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', code: 'Enter', keyCode: 13, bubbles: true }));
            return { ok: true, strategy: 'el-datepicker' };
        },

        // 智能元素定位：根据多种格式自动选择定位策略
        locateElement: function(selector) {
            if (!selector) return null;
            selector = selector.trim();

            // 策略 1: rect:x,y,w,h — 批注矩形区域定位
            if (selector.indexOf('rect:') === 0) {
                var parts = selector.substring(5).split(',').map(Number);
                if (parts.length >= 4 && parts.every(function(n) { return !isNaN(n); })) {
                    return this._findElementInRect(parts[0], parts[1], parts[2], parts[3]);
                }
                return null;
            }

            // 策略 2: aria:label — ARIA 属性定位
            if (selector.indexOf('aria:') === 0) {
                var ariaVal = selector.substring(5);
                var el = document.querySelector('[aria-label*="' + ariaVal + '"]');
                if (el) return el;
                el = document.querySelector('[role="' + ariaVal + '"]');
                if (el) return el;
                el = document.querySelector('[aria-roledescription*="' + ariaVal + '"]');
                return el || null;
            }

            // 策略 3: CSS selector 尝试（#id, .class, tag, [attr], 组合选择器）
            try {
                var cssEl = document.querySelector(selector);
                if (cssEl) return cssEl;
            } catch(e) {
                // 不是有效 CSS selector，继续其他策略
            }

            // 策略 4: XPath 文本匹配 — //*[contains(text(), '...')]
            var xpathResult = document.evaluate(
                './/*[contains(text(), ' + this._xpathLiteral(selector) + ')]',
                document.body,
                null,
                XPathResult.FIRST_ORDERED_NODE_TYPE,
                null
            );
            if (xpathResult.singleNodeValue) return xpathResult.singleNodeValue;

            // 策略 5: 按钮文本模糊匹配
            var buttons = document.querySelectorAll('button, a, [role="button"], input[type="submit"]');
            var lowerSelector = selector.toLowerCase();
            for (var bi = 0; bi < buttons.length; bi++) {
                var btnText = (buttons[bi].textContent || '').trim().toLowerCase();
                var btnTitle = (buttons[bi].getAttribute('title') || '').toLowerCase();
                var btnAria = (buttons[bi].getAttribute('aria-label') || '').toLowerCase();
                if (btnText.indexOf(lowerSelector) >= 0 ||
                    btnTitle.indexOf(lowerSelector) >= 0 ||
                    btnAria.indexOf(lowerSelector) >= 0) {
                    return buttons[bi];
                }
            }

            return null;
        },

        _xpathLiteral: function(s) {
            if (s.indexOf('"') === -1) return '"' + s + '"';
            if (s.indexOf("'") === -1) return "'" + s + "'";
            var parts = s.split('"');
            return 'concat("' + parts.join('", \'"\', "') + '")';
        },

        _findElementInRect: function(x, y, w, h) {
            var candidates = [];
            var allEls = document.querySelectorAll('*');
            for (var i = 0; i < allEls.length; i++) {
                var el = allEls[i];
                var r = el.getBoundingClientRect();
                if (r.width === 0 || r.height === 0) continue;
                // 计算重叠面积
                var ox = Math.max(0, Math.min(x + w, r.left + r.width) - Math.max(x, r.left));
                var oy = Math.max(0, Math.min(y + h, r.top + r.height) - Math.max(y, r.top));
                var overlap = ox * oy;
                if (overlap > 0) {
                    candidates.push({ el: el, overlap: overlap, area: r.width * r.height });
                }
            }
            if (candidates.length === 0) return null;
            // 按重叠率排序，优先选择重叠比高且面积适中的元素
            candidates.sort(function(a, b) {
                var ratioA = a.overlap / Math.max(a.area, 1);
                var ratioB = b.overlap / Math.max(b.area, 1);
                return ratioB - ratioA;
            });
            // 跳过过大的容器（body, html 等），取第一个有意义的元素
            for (var ci = 0; ci < candidates.length; ci++) {
                var tag = candidates[ci].el.tagName.toLowerCase();
                if (tag !== 'body' && tag !== 'html' && candidates[ci].area < (w * h * 10)) {
                    return candidates[ci].el;
                }
            }
            return candidates[0].el;
        },

        // 根据元素生成 CSS selector
        generateSelector: function(el) {
            if (!el) return '';
            if (el.id) return '#' + el.id.replace(/([^\w-])/g, '\\$1');
            // 尝试 name 属性
            if (el.name) return '[name="' + el.name + '"]';
            // 向上生成路径
            var parts = [];
            var cur = el;
            while (cur && cur !== document.body) {
                var seg = cur.tagName.toLowerCase();
                if (cur.id) {
                    seg = '#' + cur.id.replace(/([^\w-])/g, '\\$1');
                    parts.unshift(seg);
                    break;
                }
                // 同级同名元素中排第几
                if (cur.parentElement) {
                    var siblings = cur.parentElement.children;
                    var sameTag = [];
                    for (var i = 0; i < siblings.length; i++) {
                        if (siblings[i].tagName === cur.tagName) sameTag.push(siblings[i]);
                    }
                    if (sameTag.length > 1) {
                        var idx = sameTag.indexOf(cur) + 1;
                        seg += ':nth-of-type(' + idx + ')';
                    }
                }
                parts.unshift(seg);
                cur = cur.parentElement;
            }
            return parts.join(' > ');
        },

        // 提取矩形区域内的 DOM 元素信息
        extractElementsInRect: function(x, y, w, h) {
            var results = [];
            var allEls = document.querySelectorAll('*');
            for (var i = 0; i < allEls.length; i++) {
                var el = allEls[i];
                var r = el.getBoundingClientRect();
                if (r.width === 0 || r.height === 0) continue;
                var ox = Math.max(0, Math.min(x + w, r.left + r.width) - Math.max(x, r.left));
                var oy = Math.max(0, Math.min(y + h, r.top + r.height) - Math.max(y, r.top));
                var overlap = ox * oy;
                if (overlap === 0) continue;
                var tag = el.tagName.toLowerCase();
                // 跳过无意义元素
                if (tag === 'html' || tag === 'body' || tag === 'head' || tag === 'script' || tag === 'style') continue;
                var overlapRatio = overlap / (r.width * r.height);
                if (overlapRatio < 0.5) continue;
                var text = (el.textContent || '').trim();
                if (text.length > 200) text = text.substring(0, 200) + '...';
                // 收集关键属性
                var attrs = {};
                var importantAttrs = ['id', 'class', 'name', 'type', 'href', 'src', 'placeholder',
                    'aria-label', 'aria-role', 'role', 'title', 'alt', 'value', 'data-testid'];
                for (var ai = 0; ai < importantAttrs.length; ai++) {
                    var val = el.getAttribute(importantAttrs[ai]);
                    if (val) attrs[importantAttrs[ai]] = val;
                }
                results.push({
                    tag: tag,
                    text: text,
                    attributes: attrs,
                    selector: this.generateSelector(el),
                    rect: { x: Math.round(r.left), y: Math.round(r.top), width: Math.round(r.width), height: Math.round(r.height) },
                    overlapRatio: Math.round(overlapRatio * 100) / 100,
                    area: Math.round(r.width * r.height)
                });
            }
            // 按面积从小到大排序（小面积 = 更精确的元素）
            results.sort(function(a, b) { return a.area - b.area; });
            // 去重：如果父元素和子元素重叠比相同，保留子元素
            var deduped = [];
            var seen = {};
            for (var ri = 0; ri < results.length; ri++) {
                var key = results[ri].selector;
                if (!seen[key]) {
                    seen[key] = true;
                    deduped.push(results[ri]);
                }
            }
            return deduped.slice(0, 20);
        },

        clickElement: function(selector) {
            var el = this.locateElement(selector);
            if (!el) return { ok: false, error: '元素未找到: ' + selector };
            el.scrollIntoView({ block: 'center', behavior: 'instant' });
            var rect = el.getBoundingClientRect();
            var x = rect.left + rect.width / 2;
            var y = rect.top + rect.height / 2;
            var opts = { bubbles: true, cancelable: true, view: window,
                         clientX: x, clientY: y, screenX: x, screenY: y,
                         button: 0, buttons: 1 };
            el.dispatchEvent(new MouseEvent('mouseover', opts));
            el.dispatchEvent(new MouseEvent('mouseenter', opts));
            el.dispatchEvent(new MouseEvent('mousemove', opts));
            el.dispatchEvent(new MouseEvent('mousedown', opts));
            el.dispatchEvent(new MouseEvent('mouseup', opts));
            el.dispatchEvent(new MouseEvent('click', opts));
            return { ok: true, x: Math.round(x), y: Math.round(y) };
        },

        annotation: {
            _active: false,
            _canvas: null,
            _ctx: null,
            _annotations: [],
            _currentTool: 'rect',
            _startX: 0,
            _startY: 0,
            _color: '#ff0000',
            _strokeWidth: 2,

            start: function(tool) {
                if (this._active) return { ok: true, note: 'already active' };
                this._currentTool = tool || 'rect';
                this._active = true;
                this._ensureCanvas();
                this._bindEvents();
                return { ok: true };
            },

            stop: function() {
                if (!this._active) return { ok: true, note: 'not active' };
                this._active = false;
                this._unbindEvents();
                if (this._canvas && this._canvas.parentNode) {
                    this._canvas.parentNode.removeChild(this._canvas);
                }
                this._canvas = null;
                this._ctx = null;
                return { ok: true };
            },

            clear: function() {
                this._annotations = [];
                this._render();
                return { ok: true };
            },

            getAnnotations: function() {
                return { annotations: this._annotations, count: this._annotations.length };
            },

            extractAnnotatedElements: function() {
                var bridge = window.__tiangong_bridge;
                if (!bridge) return { elements: [], count: 0 };
                var allElements = [];
                for (var i = 0; i < this._annotations.length; i++) {
                    var a = this._annotations[i];
                    if (a.type !== 'rect') continue;
                    var elements = bridge.extractElementsInRect(a.x, a.y, a.width, a.height);
                    allElements.push({
                        annotationIndex: i,
                        rect: { x: a.x, y: a.y, width: a.width, height: a.height },
                        elements: elements
                    });
                }
                return { elements: allElements, count: allElements.length };
            },

            _ensureCanvas: function() {
                if (this._canvas) return;
                var c = document.createElement('canvas');
                c.id = '__tiangong_annotation_canvas';
                c.style.cssText = 'position:fixed;top:0;left:0;width:100%;height:100%;z-index:2147483647;pointer-events:auto;cursor:crosshair;';
                document.body.appendChild(c);
                this._canvas = c;
                this._ctx = c.getContext('2d');
                var self = this;
                this._resizeHandler = function() { self._resize(); };
                window.addEventListener('resize', this._resizeHandler);
                this._resize();
            },

            _resize: function() {
                if (!this._canvas) return;
                this._canvas.width = window.innerWidth;
                this._canvas.height = window.innerHeight;
                this._render();
            },

            _bindEvents: function() {
                var self = this;
                this._onDown = function(e) { self._handleDown(e); };
                this._onMove = function(e) { self._handleMove(e); };
                this._onUp = function(e) { self._handleUp(e); };
                this._canvas.addEventListener('mousedown', this._onDown);
                this._canvas.addEventListener('mousemove', this._onMove);
                this._canvas.addEventListener('mouseup', this._onUp);
            },

            _unbindEvents: function() {
                if (this._canvas) {
                    this._canvas.removeEventListener('mousedown', this._onDown);
                    this._canvas.removeEventListener('mousemove', this._onMove);
                    this._canvas.removeEventListener('mouseup', this._onUp);
                }
                if (this._resizeHandler) {
                    window.removeEventListener('resize', this._resizeHandler);
                }
            },

            _handleDown: function(e) {
                this._startX = e.clientX;
                this._startY = e.clientY;
                this._drawing = true;
            },

            _handleMove: function(e) {
                if (!this._drawing) return;
                this._render();
                var ctx = this._ctx;
                ctx.strokeStyle = this._color;
                ctx.lineWidth = this._strokeWidth;
                if (this._currentTool === 'rect') {
                    ctx.strokeRect(this._startX, this._startY, e.clientX - this._startX, e.clientY - this._startY);
                } else if (this._currentTool === 'arrow') {
                    this._drawArrow(ctx, this._startX, this._startY, e.clientX, e.clientY);
                }
            },

            _handleUp: function(e) {
                if (!this._drawing) return;
                this._drawing = false;
                var annotation = {
                    type: this._currentTool,
                    color: this._color
                };
                if (this._currentTool === 'rect') {
                    annotation.x = Math.min(this._startX, e.clientX);
                    annotation.y = Math.min(this._startY, e.clientY);
                    annotation.width = Math.abs(e.clientX - this._startX);
                    annotation.height = Math.abs(e.clientY - this._startY);
                } else if (this._currentTool === 'arrow') {
                    annotation.x1 = this._startX;
                    annotation.y1 = this._startY;
                    annotation.x2 = e.clientX;
                    annotation.y2 = e.clientY;
                }
                if (annotation.width > 5 || annotation.height > 5 || annotation.x1 !== undefined) {
                    // 自动提取矩形区域内的 DOM 元素信息
                    if (annotation.type === 'rect' && window.__tiangong_bridge) {
                        try {
                            annotation.elements = window.__tiangong_bridge.extractElementsInRect(
                                annotation.x, annotation.y, annotation.width, annotation.height
                            );
                        } catch(ex) {}
                    }
                    this._annotations.push(annotation);
                }
                this._render();
            },

            _drawArrow: function(ctx, x1, y1, x2, y2) {
                var headLen = 10;
                var angle = Math.atan2(y2 - y1, x2 - x1);
                ctx.beginPath();
                ctx.moveTo(x1, y1);
                ctx.lineTo(x2, y2);
                ctx.lineTo(x2 - headLen * Math.cos(angle - Math.PI / 6), y2 - headLen * Math.sin(angle - Math.PI / 6));
                ctx.moveTo(x2, y2);
                ctx.lineTo(x2 - headLen * Math.cos(angle + Math.PI / 6), y2 - headLen * Math.sin(angle + Math.PI / 6));
                ctx.stroke();
            },

            _render: function() {
                if (!this._ctx) return;
                this._ctx.clearRect(0, 0, this._canvas.width, this._canvas.height);
                for (var i = 0; i < this._annotations.length; i++) {
                    var a = this._annotations[i];
                    this._ctx.strokeStyle = a.color;
                    this._ctx.lineWidth = 2;
                    if (a.type === 'rect') {
                        this._ctx.strokeRect(a.x, a.y, a.width, a.height);
                    } else if (a.type === 'arrow') {
                        this._drawArrow(this._ctx, a.x1, a.y1, a.x2, a.y2);
                    }
                }
            }
        },
    };

    // --- 5. 拦截 target="_blank" 链接和 window.open ---
    // 嵌入浏览器只有一个 WebView，所有导航都在同一视图内完成。
    // window.open 重写为 location.href 导航
    window.open = function(url) {
        if (url) window.location.href = url;
        return null;
    };
    // 点击事件委托：拦截 target="_blank" 的 <a> 标签
    document.addEventListener('click', function(e) {
        var el = e.target;
        // 向上查找最近的 <a> 元素
        while (el && el.tagName !== 'A') {
            el = el.parentElement;
        }
        if (!el) return;
        var target = el.getAttribute('target');
        if (target === '_blank') {
            e.preventDefault();
            e.stopPropagation();
            var href = el.getAttribute('href');
            if (href && href.indexOf('javascript:') !== 0) {
                window.location.href = href;
            }
        }
    }, true);

    console.log('[Tiangong Bridge] loaded v0.8.0');
})();
