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
            return _origFetch.call(this, input, init);
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
            // 无 <form> 时，尝试在 dialog 或 body 中查找
            if (containers.length === 0) {
                var dialog = this._getTopmostOverlay();
                containers = dialog ? [dialog] : [document.body];
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
                        // 向上查找 ds-form-item__label-text、ant-form-item-label、el-form-item__label
                        var labelEl = el.closest('[class*="form-item"]') || el.closest('.ant-form-item') || el.closest('.el-form-item');
                        if (labelEl) {
                            var labelText = labelEl.querySelector('[class*="label-text"]') ||
                                           labelEl.querySelector('.ant-form-item-label') ||
                                           labelEl.querySelector('.el-form-item__label') ||
                                           labelEl.querySelector('label');
                            if (labelText) field.label = (labelText.textContent || '').trim();
                        }
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
                    } else if (el.placeholder) {
                        field.selector = '[placeholder="' + el.placeholder.replace(/"/g, '\\"') + '"]';
                    } else {
                        field.selector = this.generateSelector(el);
                    }
                    fields.push(field);
                }
                // 提取表单内/容器内的按钮（包含原生和 div[role="button"] 等）
                var buttons = [];
                var btns = container.querySelectorAll('button, input[type="submit"], input[type="reset"], [role="button"]');
                for (var bci = 0; bci < btns.length; bci++) {
                    var btn = btns[bci];
                    var btnText = (btn.textContent || '').trim();
                    if (!btnText) continue; // 跳过无文本的按钮（如图标按钮）
                    buttons.push({
                        tag: btn.tagName.toLowerCase(),
                        type: btn.type || '',
                        text: btnText,
                        disabled: this._isDisabled(btn),
                        selector: this.generateSelector(btn)
                    });
                }
                if (fields.length > 0 || buttons.length > 0) {
                    forms.push({ fields: fields, buttons: buttons });
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
                el.focus();
                el.value = value;
                el.dispatchEvent(new Event('change', { bubbles: true }));
                el.dispatchEvent(new FocusEvent('blur', { bubbles: true }));
                return { ok: true, strategy: 'select-change' };
            }

            // checkbox / radio 特殊处理
            if (el.type === 'checkbox' || el.type === 'radio') {
                var shouldCheck = (value === 'true' || value === '1');
                if (el.checked !== shouldCheck) {
                    el.click();
                }
                el.dispatchEvent(new FocusEvent('blur', { bubbles: true }));
                return { ok: true, strategy: 'click-toggle' };
            }

            strategy = strategy || 'auto';
            var result = null;

            // 策略 1: execCommand insertText（走浏览器原生编辑管线，产生 trusted 事件）
            if (strategy === 'auto' || strategy === 'insertText') {
                el.dispatchEvent(new FocusEvent('focus', { bubbles: true }));
                el.focus();
                el.select();
                try {
                    if (document.execCommand('insertText', false, value)) {
                        if (el.value === value) {
                            result = { ok: true, strategy: 'execCommand-insertText' };
                        }
                    }
                } catch(e) {}
            }

            // 策略 2: native setter（适用于 React 受控组件）
            if (!result && (strategy === 'auto' || strategy === 'native')) {
                el.dispatchEvent(new FocusEvent('focus', { bubbles: true }));
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
                    descriptor.set.call(el, '');
                    el.dispatchEvent(new Event('input', { bubbles: true }));
                    descriptor.set.call(el, value);
                    el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: value }));
                    el.dispatchEvent(new Event('change', { bubbles: true }));
                    // 额外触发键盘事件，确保框架感知到用户交互
                    el.dispatchEvent(new KeyboardEvent('keyup', { key: ' ', code: 'Space', keyCode: 32, bubbles: true }));
                    if (el.value === value) {
                        result = { ok: true, strategy: 'native-setter' };
                    }
                }
            }

            // 策略 3: 直接赋值 + 事件（兜底）
            if (!result && (strategy === 'auto' || strategy === 'paste')) {
                el.dispatchEvent(new FocusEvent('focus', { bubbles: true }));
                el.focus();
                el.value = value;
                el.dispatchEvent(new Event('input', { bubbles: true }));
                el.dispatchEvent(new Event('change', { bubbles: true }));
                result = { ok: true, strategy: 'direct-assign' };
            }

            if (!result) {
                return { ok: false, error: '所有填写策略均未成功', currentValue: el.value };
            }

            // 填写完成后触发 blur 激活表单校验
            el.dispatchEvent(new FocusEvent('blur', { bubbles: true }));

            // 检查填写后是否有 disabled 按钮变为 enabled（返回提示信息）
            result.currentValue = el.value;
            var dialog = this._getTopmostOverlay();
            if (dialog) {
                var disabledBtns = dialog.querySelectorAll('[role="button"]');
                for (var di = 0; di < disabledBtns.length; di++) {
                    var btnText = (disabledBtns[di].textContent || '').trim();
                    if (btnText && this._isDisabled(disabledBtns[di])) {
                        result.note = '按钮 "' + btnText + '" 仍处于禁用状态，可能需要填写更多必填字段';
                        break;
                    }
                }
            }
            return result;
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
            if (!selector || typeof selector !== 'string') return null;
            selector = selector.trim();
            if (!selector) return null;

            // 策略 0: nth:N,selector — 选择第 N 个匹配
            if (selector.indexOf('nth:') === 0) {
                var commaIdx = selector.indexOf(',');
                if (commaIdx > 4) {
                    var idx = parseInt(selector.substring(4, commaIdx), 10);
                    var innerSelector = selector.substring(commaIdx + 1);
                    if (!isNaN(idx) && idx > 0) {
                        var all = this.locateAll(innerSelector);
                        if (all && all.length >= idx) {
                            return all[idx - 1];
                        }
                    }
                }
                return null;
            }

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

            // 检测当前打开的对话框，优先在其中查找
            var dialog = this._getTopmostOverlay();
            var contexts = dialog ? [dialog, document.body] : [document.body];
            var lowerSelector = selector.toLowerCase();
            var xpathLit = this._xpathLiteral(selector);

            // 策略 4: 精确文本匹配（对话框内 → 全局）
            for (var ci = 0; ci < contexts.length; ci++) {
                var exactResult = document.evaluate(
                    './/*[text() = ' + xpathLit + ']',
                    contexts[ci], null, XPathResult.FIRST_ORDERED_NODE_TYPE, null
                );
                if (exactResult.singleNodeValue) return exactResult.singleNodeValue;
            }

            // 策略 5: 按钮精确文本（对话框内 → 全局）
            var allButtons = document.querySelectorAll('button, a, [role="button"], input[type="submit"]');
            for (var ci2 = 0; ci2 < contexts.length; ci2++) {
                for (var bi = 0; bi < allButtons.length; bi++) {
                    if (!contexts[ci2].contains(allButtons[bi])) continue;
                    if ((allButtons[bi].textContent || '').trim().toLowerCase() === lowerSelector) {
                        return allButtons[bi];
                    }
                }
            }

            // 策略 6: 部分文本匹配（对话框内 → 全局）
            for (var ci3 = 0; ci3 < contexts.length; ci3++) {
                var partialResult = document.evaluate(
                    './/*[contains(text(), ' + xpathLit + ')]',
                    contexts[ci3], null, XPathResult.FIRST_ORDERED_NODE_TYPE, null
                );
                if (partialResult.singleNodeValue) return partialResult.singleNodeValue;
            }

            // 策略 7: 按钮模糊匹配（对话框内 → 全局）
            for (var ci4 = 0; ci4 < contexts.length; ci4++) {
                for (var bj = 0; bj < allButtons.length; bj++) {
                    if (!contexts[ci4].contains(allButtons[bj])) continue;
                    var btnText = (allButtons[bj].textContent || '').trim().toLowerCase();
                    var btnTitle = (allButtons[bj].getAttribute('title') || '').toLowerCase();
                    var btnAria = (allButtons[bj].getAttribute('aria-label') || '').toLowerCase();
                    if (btnText.indexOf(lowerSelector) >= 0 ||
                        btnTitle.indexOf(lowerSelector) >= 0 ||
                        btnAria.indexOf(lowerSelector) >= 0) {
                        return allButtons[bj];
                    }
                }
            }

            return null;
        },

        // 返回所有匹配的元素列表（用于候选提示和 nth 语法）
        locateAll: function(selector) {
            if (!selector || typeof selector !== 'string') return [];
            selector = selector.trim();
            if (!selector) return [];

            var results = [];
            var seen = {};
            var lowerSelector = selector.toLowerCase();

            // CSS selector
            try {
                var cssAll = document.querySelectorAll(selector);
                for (var ci = 0; ci < cssAll.length; ci++) {
                    var key = this._elementKey(cssAll[ci]);
                    if (!seen[key]) {
                        seen[key] = true;
                        results.push(cssAll[ci]);
                    }
                }
            } catch(e) {}

            // XPath exact text
            var exactIter = document.evaluate(
                './/*[text() = ' + this._xpathLiteral(selector) + ']',
                document.body, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null
            );
            for (var ei = 0; ei < exactIter.snapshotLength; ei++) {
                var key2 = this._elementKey(exactIter.snapshotItem(ei));
                if (!seen[key2]) { seen[key2] = true; results.push(exactIter.snapshotItem(ei)); }
            }

            // XPath partial text
            var partialIter = document.evaluate(
                './/*[contains(text(), ' + this._xpathLiteral(selector) + ')]',
                document.body, null, XPathResult.ORDERED_NODE_SNAPSHOT_TYPE, null
            );
            for (var pi = 0; pi < partialIter.snapshotLength; pi++) {
                var key3 = this._elementKey(partialIter.snapshotItem(pi));
                if (!seen[key3]) { seen[key3] = true; results.push(partialIter.snapshotItem(pi)); }
            }

            // Button text match
            var buttons = document.querySelectorAll('button, a, [role="button"], input[type="submit"]');
            for (var bi = 0; bi < buttons.length; bi++) {
                var key4 = this._elementKey(buttons[bi]);
                if (seen[key4]) continue;
                var btnText = (buttons[bi].textContent || '').trim().toLowerCase();
                var btnTitle = (buttons[bi].getAttribute('title') || '').toLowerCase();
                var btnAria = (buttons[bi].getAttribute('aria-label') || '').toLowerCase();
                if (btnText.indexOf(lowerSelector) >= 0 ||
                    btnTitle.indexOf(lowerSelector) >= 0 ||
                    btnAria.indexOf(lowerSelector) >= 0) {
                    seen[key4] = true;
                    results.push(buttons[bi]);
                }
            }

            return results;
        },

        _elementKey: function(el) {
            if (!el) return '';
            return el.tagName + ':' + (el.id || '') + ':' + this.generateSelector(el);
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

        queryDom: function(selector, maxResults) {
            maxResults = maxResults || 20;
            var els = document.querySelectorAll(selector);
            var results = [];
            for (var i = 0; i < Math.min(els.length, maxResults); i++) {
                var el = els[i];
                var text = (el.innerText || '').trim();
                if (text.length > 500) text = text.substring(0, 500) + '...';
                var attrs = {};
                var importantAttrs = ['id','class','name','type','href','src','placeholder',
                    'aria-label','role','title','alt','value','data-testid','disabled','readonly'];
                for (var ai = 0; ai < importantAttrs.length; ai++) {
                    var val = el.getAttribute(importantAttrs[ai]);
                    if (val) attrs[importantAttrs[ai]] = val;
                }
                var r = el.getBoundingClientRect();
                results.push({
                    index: i,
                    tag: el.tagName.toLowerCase(),
                    text: text,
                    attributes: attrs,
                    selector: this.generateSelector(el),
                    rect: { x: Math.round(r.left), y: Math.round(r.top), width: Math.round(r.width), height: Math.round(r.height) }
                });
            }
            return { selector: selector, total: els.length, returned: results.length, elements: results };
        },

        // 综合检测元素是否处于禁用状态
        _isDisabled: function(el) {
            if (!el) return false;
            // 原生 disabled 属性
            if (el.disabled) return true;
            if (el.hasAttribute && el.hasAttribute('disabled')) return true;
            // aria-disabled
            if (el.getAttribute && el.getAttribute('aria-disabled') === 'true') return true;
            // CSS 类名包含 disabled 模式（如 ds-button--disabled、is-disabled、ant-btn-disabled）
            if (el.classList) {
                for (var i = 0; i < el.classList.length; i++) {
                    var cls = el.classList[i].toLowerCase();
                    if (cls.indexOf('disabled') >= 0 || cls.indexOf('-disabled') >= 0) {
                        // 排除误判：如 "not-disabled"
                        if (cls.indexOf('not-disabled') === -1) return true;
                    }
                }
            }
            return false;
        },

        clickElement: function(selector) {
            var el = this.locateElement(selector);
            if (!el) {
                // 尝试 locateAll 看是否有候选
                var all = this.locateAll(selector);
                if (all.length > 0) {
                    return {
                        ok: false,
                        error: '元素未找到: ' + selector,
                        candidates: all.slice(0, 5).map(function(c) {
                            return {
                                tag: (c.tagName || '').toLowerCase(),
                                text: (c.textContent || '').trim().substring(0, 50),
                                selector: window.__tiangong_bridge.generateSelector(c)
                            };
                        })
                    };
                }
                return { ok: false, error: '元素未找到: ' + selector, candidates: [] };
            }

            // 如果找到的是内联元素（如 span），尝试点击其父级按钮/链接
            var clickTarget = el;
            var interactiveTags = ['BUTTON', 'A', 'INPUT', 'SELECT', 'TEXTAREA', 'SUMMARY', 'LABEL'];
            if (interactiveTags.indexOf(el.tagName) === -1) {
                var parent = el.closest('button, a, [role="button"], label, summary');
                if (parent) clickTarget = parent;
            }

            // 检测 disabled 状态
            if (this._isDisabled(clickTarget)) {
                return { ok: false, error: '元素已禁用: ' + selector, candidates: [] };
            }

            clickTarget.scrollIntoView({ block: 'center', behavior: 'instant' });
            clickTarget.focus();

            // 使用 el.click() 触发浏览器原生点击，产生 trusted 事件
            clickTarget.click();
            return { ok: true, candidates: [] };
        },

        // 等待页面条件满足（异步，返回 Promise）
        // condition: 'navigation' | 'element:selector' | 'element!:selector' | 'stable'
        waitFor: function(condition, timeoutMs) {
            var self = this;
            var startTime = Date.now();
            var timeout = timeoutMs || 5000;
            // 记录初始状态
            this._waitInitialState = {
                url: window.location.href,
                lastMutationTime: Date.now()
            };
            // 启动 MutationObserver 追踪 DOM 变化（用于 stable 条件）
            if (condition === 'stable' && !this._waitObserver) {
                this._startMutationObserver();
            }
            return new Promise(function(resolve) {
                var check = function() {
                    if (self._checkWaitCondition(condition)) {
                        self._stopMutationObserver();
                        resolve({ ok: true, condition: condition, elapsed: Date.now() - startTime });
                        return;
                    }
                    if (Date.now() - startTime > timeout) {
                        self._stopMutationObserver();
                        resolve({ ok: false, error: '等待超时', condition: condition, elapsed: Date.now() - startTime });
                        return;
                    }
                    setTimeout(check, 200);
                };
                check();
            });
        },

        _checkWaitCondition: function(condition) {
            if (condition === 'navigation') {
                return window.location.href !== this._waitInitialState.url;
            }
            if (condition === 'stable') {
                return Date.now() - this._waitInitialState.lastMutationTime > 1000;
            }
            if (condition.indexOf('element!:') === 0) {
                var sel = condition.substring(9);
                try { return !document.querySelector(sel); } catch(e) { return true; }
            }
            if (condition.indexOf('element:') === 0) {
                var sel2 = condition.substring(8);
                try { return !!document.querySelector(sel2); } catch(e) { return false; }
            }
            return false;
        },

        // 获取页面摘要（用于操作前后对比）
        getPageDigest: function() {
            var overlay = this._getTopmostOverlay();
            var fullText = (document.body.innerText || '').replace(/\s+/g, ' ').trim();
            return {
                url: window.location.href,
                title: document.title,
                overlayOpen: !!overlay,
                overlayText: overlay ? this._extractOverlayContent(overlay) : '',
                mainTextTail: fullText.length > 3000 ? fullText.substring(fullText.length - 3000) : fullText
            };
        },

        // 对比前后摘要，返回差异描述
        diffDigest: function(before, after) {
            if (!before || !after) return '';
            var changes = [];
            if (before.url !== after.url) {
                changes.push('页面已导航到 ' + after.url);
            }
            if (!before.overlayOpen && after.overlayOpen) {
                changes.push('覆盖层已出现：' + after.overlayText.substring(0, 2000));
            }
            if (before.overlayOpen && !after.overlayOpen) {
                changes.push('覆盖层已关闭');
            }
            if (before.overlayOpen && after.overlayOpen && before.overlayText !== after.overlayText) {
                changes.push('覆盖层内容已变化：' + after.overlayText.substring(0, 2000));
            }
            // 页面尾部内容变化
            if (before.mainTextTail !== after.mainTextTail && after.mainTextTail) {
                var newContent = this._textDiff(before.mainTextTail, after.mainTextTail);
                if (newContent) {
                    changes.push('页面内容变化：' + newContent);
                }
            }
            if (changes.length === 0) {
                changes.push('页面无明显变化');
            }
            return changes.join('\n');
        },

        // 简单文本差异：提取 after 中新增的内容
        _textDiff: function(before, after) {
            if (!before || !after) return after ? after.substring(0, 500) : '';
            // 按句子/片段分割
            var beforeParts = before.split(/(?<=[。，！？；\n.!?;])|(?<=\s{2,})/);
            var afterParts = after.split(/(?<=[。，！？；\n.!?;])|(?<=\s{2,})/);
            var beforeSet = {};
            for (var i = 0; i < beforeParts.length; i++) {
                var p = beforeParts[i].trim();
                if (p.length > 3) beforeSet[p] = true;
            }
            var newParts = [];
            for (var j = 0; j < afterParts.length; j++) {
                var ap = afterParts[j].trim();
                if (ap.length > 3 && !beforeSet[ap]) {
                    newParts.push(ap);
                }
            }
            if (newParts.length === 0) return '';
            var result = newParts.join(' ').trim();
            return result.length > 1000 ? result.substring(0, 1000) + '...' : result;
        },

        _startMutationObserver: function() {
            var self = this;
            this._waitObserver = new MutationObserver(function() {
                if (self._waitInitialState) {
                    self._waitInitialState.lastMutationTime = Date.now();
                }
            });
            this._waitObserver.observe(document.body, {
                childList: true, subtree: true, characterData: true, attributes: true
            });
        },

        _stopMutationObserver: function() {
            if (this._waitObserver) {
                this._waitObserver.disconnect();
                this._waitObserver = null;
            }
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

        // ── 持久观测层 ─────────────────────────────────────
        observer: {
            _eventQueue: [],
            _mutationObserver: null,
            _debounceTimer: null,
            _pendingMutations: [],
            _started: false,
            _userEventBound: false,

            start: function() {
                if (this._started) return;
                this._started = true;
                this._startMutationObserver();
                this._bindUserEvents();
            },

            stop: function() {
                this._started = false;
                if (this._mutationObserver) {
                    this._mutationObserver.disconnect();
                    this._mutationObserver = null;
                }
                if (this._debounceTimer) {
                    clearTimeout(this._debounceTimer);
                    this._debounceTimer = null;
                }
                this._pendingMutations = [];
            },

            drainEvents: function() {
                var events = this._eventQueue;
                this._eventQueue = [];
                return events;
            },

            // ── MutationObserver ──

            _startMutationObserver: function() {
                var self = this;
                this._mutationObserver = new MutationObserver(function(mutations) {
                    if (!self._started) return;
                    self._pendingMutations = self._pendingMutations.concat(mutations);
                    if (self._debounceTimer) clearTimeout(self._debounceTimer);
                    self._debounceTimer = setTimeout(function() {
                        self._flushMutations();
                    }, 500);
                });
                this._mutationObserver.observe(document.body, {
                    childList: true,
                    subtree: true,
                    attributes: true,
                    attributeFilter: [
                        'class', 'style', 'disabled', 'readonly',
                        'aria-busy', 'aria-hidden', 'aria-disabled',
                        'aria-expanded', 'aria-selected'
                    ]
                });
            },

            _flushMutations: function() {
                var mutations = this._pendingMutations;
                this._pendingMutations = [];
                var events = this._analyzeMutations(mutations);
                for (var i = 0; i < events.length; i++) {
                    this._pushEvent(events[i]);
                }
            },

            _analyzeMutations: function(mutations) {
                var events = [];
                var dialogAdded = false;
                var dialogRemoved = false;
                var contentChanged = false;

                for (var i = 0; i < mutations.length; i++) {
                    var m = mutations[i];

                    if (m.type === 'attributes') {
                        continue;
                    }

                    if (m.type === 'childList') {
                        for (var j = 0; j < m.addedNodes.length; j++) {
                            var node = m.addedNodes[j];
                            if (node.nodeType !== 1) continue;
                            if (this._isDialog(node) || this._containsDialog(node)) {
                                dialogAdded = true;
                            }
                            if (this._isMainContent(node) || this._isMainContent(m.target)) {
                                contentChanged = true;
                            }
                        }
                        for (var k = 0; k < m.removedNodes.length; k++) {
                            var rNode = m.removedNodes[k];
                            if (rNode.nodeType !== 1) continue;
                            if (this._isDialog(rNode) || this._containsDialog(rNode)) {
                                dialogRemoved = true;
                            }
                        }
                    }
                }

                if (dialogAdded) {
                    events.push({
                        type: 'dialog_opened',
                        timestamp: Date.now(),
                        detail: this._describeActiveOverlay()
                    });
                }
                if (dialogRemoved && !dialogAdded) {
                    events.push({
                        type: 'dialog_closed',
                        timestamp: Date.now()
                    });
                }
                if (contentChanged) {
                    events.push({
                        type: 'content_changed',
                        timestamp: Date.now(),
                        detail: this._getContentSummary()
                    });
                }

                return events;
            },

            // ── 用户行为监听 ──

            _bindUserEvents: function() {
                if (this._userEventBound) return;
                this._userEventBound = true;
                var self = this;

                document.addEventListener('click', function(e) {
                    if (!self._started) return;
                    var target = e.target;
                    var interactive = target.closest(
                        'button, a, input, select, textarea, [role="button"], [role="link"], [role="tab"], summary'
                    );
                    if (!interactive) return;
                    var desc = self._describeElement(interactive);
                    self._pushEvent({
                        type: 'user_click',
                        timestamp: Date.now(),
                        element: desc.tag,
                        text: desc.text,
                        selector: desc.selector
                    });
                }, true);

                var inputTimer = null;
                var inputTarget = null;
                document.addEventListener('input', function(e) {
                    if (!self._started) return;
                    inputTarget = e.target;
                    if (inputTimer) clearTimeout(inputTimer);
                    inputTimer = setTimeout(function() {
                        if (!inputTarget) return;
                        var desc = self._describeElement(inputTarget);
                        self._pushEvent({
                            type: 'user_input',
                            timestamp: Date.now(),
                            selector: desc.selector,
                            label: desc.label || desc.placeholder,
                            value_length: (inputTarget.value || '').length
                        });
                        inputTarget = null;
                    }, 1000);
                }, true);

                window.addEventListener('popstate', function() {
                    if (!self._started) return;
                    self._pushEvent({
                        type: 'user_navigation',
                        timestamp: Date.now(),
                        url: window.location.href
                    });
                });
            },

            // ── 辅助方法 ──

            _isDialog: function(el) {
                if (el.nodeType !== 1) return false;
                if (el.getAttribute && el.getAttribute('role') === 'dialog') return true;
                // 通过几何特征检测覆盖层
                if (el.tagName) {
                    var style = window.getComputedStyle(el);
                    var pos = style.position;
                    if (pos === 'fixed' || pos === 'absolute') {
                        var rect = el.getBoundingClientRect();
                        var W = window.innerWidth;
                        var H = window.innerHeight;
                        if (rect.width > 100 && rect.height > 50 &&
                            rect.width < W - 10 && rect.height < H - 10) {
                            return true;
                        }
                    }
                }
                return false;
            },

            _containsDialog: function(el) {
                if (el.nodeType !== 1) return false;
                try {
                    var dialogs = el.querySelectorAll('[role="dialog"]');
                    if (dialogs.length > 0) return true;
                    // 检查 fixed/absolute 子元素
                    var fixed = el.querySelectorAll('*');
                    for (var i = 0; i < Math.min(fixed.length, 50); i++) {
                        var s = window.getComputedStyle(fixed[i]);
                        if ((s.position === 'fixed' || s.position === 'absolute')) {
                            var r = fixed[i].getBoundingClientRect();
                            if (r.width > 100 && r.height > 50 &&
                                r.width < window.innerWidth - 10 && r.height < window.innerHeight - 10) {
                                return true;
                            }
                        }
                    }
                } catch(e) {}
                return false;
            },

            _isMainContent: function(el) {
                if (el.nodeType !== 1) return false;
                var tag = el.tagName;
                if (tag === 'MAIN' || tag === 'ARTICLE') return true;
                if (el.id === 'app' || el.id === 'root' || el.id === '__next') return true;
                return false;
            },

            _describeElement: function(el) {
                var text = (el.textContent || '').trim().substring(0, 100);
                var tag = (el.tagName || '').toLowerCase();
                var selector = '';
                if (el.id) {
                    selector = '#' + el.id;
                } else {
                    selector = window.__tiangong_bridge.generateSelector(el);
                }
                var label = '';
                var labelEl = el.closest('[class*="form-item"]');
                if (labelEl) {
                    var lbl = labelEl.querySelector('[class*="label"]');
                    if (lbl) label = (lbl.textContent || '').trim();
                }
                var placeholder = el.placeholder || el.getAttribute('placeholder') || '';
                return { tag: tag, text: text, selector: selector, label: label, placeholder: placeholder };
            },

            _describeActiveOverlay: function() {
                var overlay = window.__tiangong_bridge._getTopmostOverlay();
                if (!overlay) return '';
                // 克隆后移除交互元素
                var clone = overlay.cloneNode(true);
                var interactive = clone.querySelectorAll('button, [role="button"], [class*="close"], [class*="Close"], [aria-label="Close"]');
                for (var i = 0; i < interactive.length; i++) interactive[i].remove();
                return (clone.innerText || '').trim().substring(0, 2000);
            },

            _getContentSummary: function() {
                var text = (document.body.innerText || '').trim();
                return text.length > 500 ? text.substring(0, 500) + '...' : text;
            },

            _pushEvent: function(event) {
                this._eventQueue.push(event);
                if (this._eventQueue.length > 100) {
                    this._eventQueue = this._eventQueue.slice(-50);
                }
            }
        },

        // ── 泛化覆盖层检测（替代 _getActiveDialog）───────────
        _getTopmostOverlay: function() {
            var W = window.innerWidth;
            var H = window.innerHeight;
            var points = [
                [Math.round(W / 2), Math.round(H / 3)],
                [Math.round(W / 2), Math.round(H / 2)]
            ];
            // 方法 1：elementFromPoint + 向上遍历找非全屏 fixed/absolute 容器
            for (var pi = 0; pi < points.length; pi++) {
                var el = document.elementFromPoint(points[pi][0], points[pi][1]);
                if (!el || el === document.body || el === document.documentElement) continue;
                var overlay = this._walkUpToOverlay(el, W, H);
                if (overlay) return overlay;
            }
            return null;
        },

        // 从命中元素向上找第一个 fixed/absolute 定位容器。
        // 如果遇到全屏蒙层，尝试在蒙层内查找非全屏子覆盖层。
        _walkUpToOverlay: function(el, W, H) {
            var current = el;
            while (current && current !== document.body && current !== document.documentElement) {
                var style = window.getComputedStyle(current);
                var pos = style.position;
                if (pos === 'fixed' || pos === 'absolute') {
                    var rect = current.getBoundingClientRect();
                    if (rect.width > 50 && rect.height > 50 &&
                        (rect.width < W - 10 || rect.height < H - 10)) {
                        return current;
                    }
                    // 全屏 fixed/absolute 容器（蒙层）：
                    // 在其内部查找非全屏的 fixed/absolute 子元素
                    var inner = this._findInnerOverlay(current, W, H);
                    if (inner) return inner;
                    // 蒙层内没有非全屏子覆盖层，检查蒙层自身是否有有意义的文本内容
                    var text = (current.innerText || '').replace(/\s+/g, ' ').trim();
                    if (text.length > 10) return current;
                    return null;
                }
                current = current.parentElement;
            }
            return null;
        },

        // 在全屏蒙层内查找面积适中的 fixed/absolute 子元素
        _findInnerOverlay: function(backdrop, W, H) {
            var candidates = backdrop.querySelectorAll('*');
            var best = null;
            var bestArea = 0;
            for (var i = 0; i < candidates.length; i++) {
                var child = candidates[i];
                var style = window.getComputedStyle(child);
                var pos = style.position;
                if (pos === 'fixed' || pos === 'absolute') {
                    var rect = child.getBoundingClientRect();
                    if (rect.width > 50 && rect.height > 50 &&
                        (rect.width < W - 10 || rect.height < H - 10)) {
                        var area = rect.width * rect.height;
                        if (area > bestArea) {
                            bestArea = area;
                            best = child;
                        }
                    }
                }
            }
            return best;
        },

        _extractOverlayContent: function(overlay) {
            if (!overlay) return '';
            var clone = overlay.cloneNode(true);
            var interactive = clone.querySelectorAll('button, [role="button"], [class*="close"], [class*="Close"], [aria-label="Close"]');
            for (var i = 0; i < interactive.length; i++) interactive[i].remove();
            return (clone.innerText || '').trim().substring(0, 3000);
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
