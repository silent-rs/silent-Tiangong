pub const BRIDGE_SCRIPT: &str = r#"
(function() {
    if (window.__tiangong_bridge_loaded) return;
    window.__tiangong_bridge_loaded = true;

    // 屏蔽 Tauri IPC — 浏览器 WebView 加载外部 URL，不应暴露 Tauri API
    try {
        var _noop = function() { return Promise.resolve(''); };
        if (window.__TAURI_INTERNALS__) {
            window.__TAURI_INTERNALS__.ipc = _noop;
            window.__TAURI_INTERNALS__.postMessage = function() {};
        }
        var _origFetch = window.fetch;
        window.fetch = function(input, init) {
            var url = (typeof input === 'string') ? input : (input && input.url ? input.url : '');
            if (url.indexOf('ipc://') === 0) {
                return Promise.resolve(new Response('{}', { status: 200 }));
            }
            return _origFetch.apply(this, arguments);
        };
        var _origXHR = window.XMLHttpRequest.prototype.open;
        window.XMLHttpRequest.prototype.open = function(method, url) {
            if (typeof url === 'string' && url.indexOf('ipc://') === 0) {
                return;
            }
            return _origXHR.apply(this, arguments);
        };
    } catch(e) {}

    window.__tiangong_bridge = {
        version: '0.7.0',

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
            return { forms: forms };
        },

        fillField: function(selector, value, strategy) {
            var el = document.querySelector(selector);
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

        clickElement: function(selector) {
            var el = document.querySelector(selector);
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
    };

    console.log('[Tiangong Bridge] loaded v0.7.0');
})();
"#;
