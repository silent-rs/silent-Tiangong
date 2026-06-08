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
        version: '0.9.0',

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

        _normalizeText: function(text) {
            return (text || '').replace(/\s+/g, ' ').trim().toLowerCase();
        },

        _shortText: function(text, maxLen) {
            maxLen = maxLen || 80;
            text = (text || '').replace(/\s+/g, ' ').trim();
            if (text.length > maxLen) {
                return text.substring(0, maxLen - 1) + '…';
            }
            return text;
        },

        _escapeCssIdent: function(value) {
            value = String(value || '');
            if (window.CSS && window.CSS.escape) {
                return window.CSS.escape(value);
            }
            return value.replace(/([^\w-])/g, '\\$1');
        },

        _escapeCssString: function(value) {
            return String(value || '').replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\a ');
        },

        _isVisible: function(el) {
            if (!el || el.nodeType !== 1) return false;
            var style = window.getComputedStyle ? window.getComputedStyle(el) : null;
            if (style && (style.display === 'none' || style.visibility === 'hidden' || style.opacity === '0')) {
                return false;
            }
            var rect = el.getBoundingClientRect ? el.getBoundingClientRect() : null;
            if (rect && rect.width <= 0 && rect.height <= 0) return false;
            return true;
        },

        _isDisabled: function(el) {
            return !!(el && (el.disabled || el.getAttribute('aria-disabled') === 'true' || el.classList.contains('disabled') || el.classList.contains('is-disabled')));
        },

        _queryAll: function(selector) {
            try {
                return Array.from(document.querySelectorAll(selector));
            } catch(e) {
                return null;
            }
        },

        _matches: function(el, selector) {
            if (!el || !el.matches) return false;
            try {
                return el.matches(selector);
            } catch(e) {
                return false;
            }
        },

        _selectorMatchesOnly: function(selector, el) {
            var matches = this._queryAll(selector);
            return !!(matches && matches.length === 1 && matches[0] === el);
        },

        _cssPath: function(el) {
            if (!el || !el.tagName) return '';
            var parts = [];
            var node = el;
            while (node && node.nodeType === 1 && node !== document) {
                var tag = node.tagName.toLowerCase();
                if (node.id) {
                    var idSelector = '#' + this._escapeCssIdent(node.id);
                    parts.unshift(idSelector);
                    break;
                }
                var parent = node.parentElement;
                if (!parent) {
                    parts.unshift(tag);
                    break;
                }
                var siblings = Array.from(parent.children).filter(function(child) {
                    return child.tagName === node.tagName;
                });
                if (siblings.length > 1) {
                    tag += ':nth-of-type(' + (siblings.indexOf(node) + 1) + ')';
                }
                parts.unshift(tag);
                node = parent;
            }
            return parts.join(' > ');
        },

        generateSelector: function(el) {
            if (!el || !el.tagName) return '';
            var tag = el.tagName.toLowerCase();
            var selector;

            if (el.id) {
                selector = '#' + this._escapeCssIdent(el.id);
                if (this._selectorMatchesOnly(selector, el)) return selector;
            }

            var testAttr = '';
            var testId = '';
            if (el.getAttribute('data-testid')) {
                testAttr = 'data-testid';
                testId = el.getAttribute('data-testid');
            } else if (el.getAttribute('data-test')) {
                testAttr = 'data-test';
                testId = el.getAttribute('data-test');
            } else if (el.getAttribute('data-cy')) {
                testAttr = 'data-cy';
                testId = el.getAttribute('data-cy');
            }
            if (testId) {
                selector = tag + '[' + testAttr + '="' + this._escapeCssString(testId) + '"]';
                if (this._selectorMatchesOnly(selector, el)) return selector;
                selector = '[' + testAttr + '="' + this._escapeCssString(testId) + '"]';
                if (this._selectorMatchesOnly(selector, el)) return selector;
            }

            if (el.name) {
                selector = tag + '[name="' + this._escapeCssString(el.name) + '"]';
                if (this._selectorMatchesOnly(selector, el)) return selector;
                selector = '[name="' + this._escapeCssString(el.name) + '"]';
                if (this._selectorMatchesOnly(selector, el)) return selector;
            }

            var aria = el.getAttribute('aria-label');
            if (aria) {
                selector = tag + '[aria-label="' + this._escapeCssString(aria) + '"]';
                if (this._selectorMatchesOnly(selector, el)) return selector;
            }

            var placeholder = el.getAttribute('placeholder');
            if (placeholder) {
                selector = tag + '[placeholder="' + this._escapeCssString(placeholder) + '"]';
                if (this._selectorMatchesOnly(selector, el)) return selector;
            }

            return this._cssPath(el);
        },

        _formLabelFor: function(el) {
            if (!el) return '';
            if (el.id) {
                var byFor = document.querySelector('label[for="' + this._escapeCssString(el.id) + '"]');
                if (byFor) return this._shortText(byFor.textContent || '');
            }
            if (el.labels && el.labels.length > 0) {
                return this._shortText(el.labels[0].textContent || '');
            }
            var closestLabel = el.closest ? el.closest('label') : null;
            if (closestLabel) return this._shortText(closestLabel.textContent || '');
            var antLabel = this._getAntFormItemLabel ? this._getAntFormItemLabel(el) : '';
            if (antLabel) return this._shortText(antLabel);
            var elLabel = this._getElFormItemLabel ? this._getElFormItemLabel(el) : '';
            if (elLabel) return this._shortText(elLabel);
            var formItem = el.closest ? el.closest('.form-item,.field,.form-group,[class*="form-item"],[class*="form-group"]') : null;
            if (formItem) {
                var label = formItem.querySelector('label');
                if (label) return this._shortText(label.textContent || '');
            }
            return '';
        },

        _implicitRole: function(el) {
            if (!el || !el.tagName) return '';
            var tag = el.tagName.toLowerCase();
            var type = (el.type || '').toLowerCase();
            if (tag === 'button' || (tag === 'input' && ['button', 'submit', 'reset'].indexOf(type) >= 0)) return 'button';
            if (tag === 'a' && el.getAttribute('href')) return 'link';
            if (tag === 'select') return 'combobox';
            if (tag === 'textarea') return 'textbox';
            if (tag === 'input') {
                if (type === 'checkbox') return 'checkbox';
                if (type === 'radio') return 'radio';
                if (type === 'range') return 'slider';
                return 'textbox';
            }
            if (tag === 'summary') return 'button';
            return '';
        },

        _accessibleName: function(el) {
            if (!el) return '';
            var aria = el.getAttribute('aria-label');
            if (aria) return this._shortText(aria);
            var labelledBy = el.getAttribute('aria-labelledby');
            if (labelledBy) {
                var texts = labelledBy.split(/\s+/).map(function(id) {
                    var ref = document.getElementById(id);
                    return ref ? (ref.textContent || '') : '';
                }).filter(Boolean);
                if (texts.length > 0) return this._shortText(texts.join(' '));
            }
            var label = this._formLabelFor(el);
            if (label) return label;
            var placeholder = el.getAttribute('placeholder');
            if (placeholder) return this._shortText(placeholder);
            var title = el.getAttribute('title');
            if (title) return this._shortText(title);
            var alt = el.getAttribute('alt');
            if (alt) return this._shortText(alt);
            if (el.value && this._matches(el, 'button,input[type="button"],input[type="submit"],input[type="reset"]')) {
                return this._shortText(el.value);
            }
            return this._shortText(el.textContent || '');
        },

        _visibleText: function(el) {
            if (!el) return '';
            return this._shortText((el.innerText || el.textContent || '').replace(/\s+/g, ' '));
        },

        _candidateSelector: function(action, includeComponents) {
            if (action === 'fill') {
                if (includeComponents === 'only') {
                    return '.ant-select,.ant-picker,.el-select,.el-date-editor';
                }
                var base = 'input:not([type="hidden"]):not([type="submit"]):not([type="button"]):not([type="reset"]),textarea,select,[contenteditable="true"]';
                if (includeComponents) {
                    base += ',.ant-select,.ant-picker,.el-select,.el-date-editor';
                }
                return base;
            }
            return 'button,a,summary,[role="button"],[role="link"],[role="tab"],[role="menuitem"],[onclick],[tabindex],input[type="button"],input[type="submit"],input[type="reset"]';
        },

        _candidateElements: function(options) {
            options = options || {};
            var selector = this._candidateSelector(
                options.action || 'click',
                options.componentOnly ? 'only' : !!options.components
            );
            var elements = this._queryAll(selector) || [];
            if ((options.action || 'click') === 'fill' && !options.components && !options.componentOnly) {
                elements = elements.filter(function(el) {
                    return !(el.closest && el.closest('.ant-select,.ant-picker,.el-select,.el-date-editor'));
                });
            }
            if ((options.action || 'click') === 'click') {
                var hasFallbackText = elements.length === 0;
                if (!hasFallbackText) return elements;
                elements = this._queryAll('body *') || [];
                elements = elements.filter(function(el) {
                    var tag = el.tagName ? el.tagName.toLowerCase() : '';
                    if (['script', 'style', 'noscript', 'meta', 'link', 'svg', 'path'].indexOf(tag) >= 0) return false;
                    var text = (el.innerText || el.textContent || '').replace(/\s+/g, ' ').trim();
                    return text.length > 0 && text.length <= 120;
                });
            }
            return elements;
        },

        _primaryQueryText: function(query) {
            query = (query || '').trim();
            var quoted = query.match(/[“‘"']([^“”‘’"']+)[”’"']/);
            if (quoted && quoted[1]) return quoted[1].trim();
            var text = query
                .replace(/^(请|帮我|帮忙)?\s*(点击|点一下|按下|打开|选择|填写|输入|填入|设置)\s*/g, '')
                .replace(/\s*(按钮|按键|链接|入口|输入框|文本框|字段|下拉框|选择框|复选框|单选框|表单项|控件|元素)$/g, '')
                .replace(/(包含|含有|文字|文本|名称|名为|叫做|为|的)/g, ' ')
                .replace(/\s+/g, ' ')
                .trim();
            return text || query;
        },

        _textScore: function(queryText, candidateText) {
            var q = this._normalizeText(queryText);
            var c = this._normalizeText(candidateText);
            if (!q || !c) return 0;
            if (q === c) return 100;
            if (c.indexOf(q) >= 0) return 82;
            if (q.indexOf(c) >= 0 && c.length >= 2) return 72;
            return 0;
        },

        _typeBoost: function(query, el, action) {
            query = query || '';
            var role = (el.getAttribute('role') || this._implicitRole(el) || '').toLowerCase();
            var tag = el.tagName ? el.tagName.toLowerCase() : '';
            var type = (el.type || '').toLowerCase();
            var boost = 0;
            if (/按钮|按键|提交|登录|确认/.test(query) && (role === 'button' || tag === 'button' || ['button', 'submit', 'reset'].indexOf(type) >= 0)) boost += 12;
            if (/链接|入口|打开/.test(query) && (role === 'link' || tag === 'a')) boost += 12;
            if (/输入框|文本框|字段|填写|输入|邮箱|账号|密码|电话|手机/.test(query) && action === 'fill') boost += 10;
            if (/下拉|选择/.test(query) && (tag === 'select' || role === 'combobox' || el.classList.contains('ant-select') || el.classList.contains('el-select'))) boost += 10;
            return boost;
        },

        _describeCandidate: function(el, score, reason) {
            var rect = el && el.getBoundingClientRect ? el.getBoundingClientRect() : null;
            return {
                selector: this.generateSelector(el),
                text: this._visibleText(el),
                tag: el && el.tagName ? el.tagName.toLowerCase() : '',
                role: (el && (el.getAttribute('role') || this._implicitRole(el))) || '',
                label: this._accessibleName(el),
                score: Math.round(score || 0),
                reason: reason || '',
                x: rect ? Math.round(rect.left + rect.width / 2) : null,
                y: rect ? Math.round(rect.top + rect.height / 2) : null
            };
        },

        _pushCandidate: function(list, seen, el, score, reason) {
            if (!el || seen.indexOf(el) >= 0 || !this._isVisible(el) || this._isDisabled(el)) return;
            seen.push(el);
            list.push({ el: el, score: score, reason: reason });
        },

        _rankElement: function(el, query, options, reason) {
            options = options || {};
            var queryText = this._primaryQueryText(query);
            var action = options.action || 'click';
            var score = 0;
            var visible = this._visibleText(el);
            var name = this._accessibleName(el);
            var label = this._formLabelFor(el);
            var placeholder = el.getAttribute('placeholder') || '';
            var attrName = el.getAttribute('name') || '';
            var id = el.id || '';

            score = Math.max(score, this._textScore(queryText, visible));
            score = Math.max(score, this._textScore(queryText, name) + (name ? 8 : 0));
            score = Math.max(score, this._textScore(queryText, label) + (label ? 12 : 0));
            score = Math.max(score, this._textScore(queryText, placeholder) + (placeholder ? 8 : 0));
            score = Math.max(score, this._textScore(queryText, attrName) + (attrName ? 4 : 0));
            score = Math.max(score, this._textScore(queryText, id) + (id ? 2 : 0));
            if (score > 0) score += this._typeBoost(query, el, action);
            if (reason === 'css selector') score = 120;
            return score;
        },

        _parseRoleQuery: function(query) {
            var m = (query || '').match(/^role\s*=\s*([^\[\]]+)(?:\[\s*name\s*=\s*['"]?([^'"\]]+)['"]?\s*\])?$/i);
            if (!m) return null;
            return { role: (m[1] || '').trim().toLowerCase(), name: (m[2] || '').trim() };
        },

        _findExplicitCandidates: function(query, options, candidates, seen) {
            var q = (query || '').trim();
            var action = (options && options.action) || 'click';
            var m = q.match(/^(text|aria|aria-label|label|placeholder|name)\s*=\s*(.+)$/i);
            if (m) {
                var kind = m[1].toLowerCase();
                if (kind === 'aria-label') kind = 'aria';
                var value = m[2].replace(/^[‘’“”'"]|[‘’“”'"]$/g, '').trim();
                var elements = this._candidateElements(options);
                for (var i = 0; i < elements.length; i++) {
                    var el = elements[i];
                    var field = '';
                    if (kind === 'text') field = this._visibleText(el) || this._accessibleName(el);
                    if (kind === 'aria') field = el.getAttribute('aria-label') || '';
                    if (kind === 'label') field = this._formLabelFor(el) || this._accessibleName(el);
                    if (kind === 'placeholder') field = el.getAttribute('placeholder') || '';
                    if (kind === 'name') field = el.getAttribute('name') || '';
                    var score = this._textScore(value, field);
                    if (score > 0) {
                        this._pushCandidate(candidates, seen, el, score + 18 + this._typeBoost(value, el, action), kind + ' match');
                    }
                }
                return;
            }

            var roleQuery = this._parseRoleQuery(q);
            if (roleQuery) {
                var all = this._queryAll('[role],button,a,input,textarea,select,summary') || [];
                for (var r = 0; r < all.length; r++) {
                    var role = (all[r].getAttribute('role') || this._implicitRole(all[r]) || '').toLowerCase();
                    if (role !== roleQuery.role) continue;
                    var score = 92;
                    if (roleQuery.name) {
                        score = this._textScore(roleQuery.name, this._accessibleName(all[r]) || this._visibleText(all[r]));
                        if (score === 0) continue;
                        score += 18;
                    }
                    this._pushCandidate(candidates, seen, all[r], score, 'role match');
                }
            }
        },

        _parseChineseNumber: function(text) {
            text = String(text || '').trim();
            if (/^\d+$/.test(text)) return parseInt(text, 10);
            var map = { '零': 0, '一': 1, '二': 2, '两': 2, '三': 3, '四': 4, '五': 5, '六': 6, '七': 7, '八': 8, '九': 9 };
            if (text === '十') return 10;
            var ten = text.indexOf('十');
            if (ten >= 0) {
                var left = text.substring(0, ten);
                var right = text.substring(ten + 1);
                return (left ? (map[left] || 0) : 1) * 10 + (right ? (map[right] || 0) : 0);
            }
            return map[text] || 0;
        },

        _findTableCellTarget: function(query, options, candidates, seen) {
            var m = (query || '').match(/(?:表格|table).*?第?\s*([一二两三四五六七八九十\d]+)\s*行.*?第?\s*([一二两三四五六七八九十\d]+)\s*列/);
            if (!m) return;
            var rowIndex = this._parseChineseNumber(m[1]) - 1;
            var colIndex = this._parseChineseNumber(m[2]) - 1;
            if (rowIndex < 0 || colIndex < 0) return;
            var wantLink = /链接|link/.test(query);
            var wantButton = /按钮|按键|button/.test(query);
            var tables = this._queryAll('table') || [];
            for (var t = 0; t < tables.length; t++) {
                if (!this._isVisible(tables[t])) continue;
                var rows = Array.from(tables[t].querySelectorAll('tr')).filter(this._isVisible.bind(this));
                if (rowIndex >= rows.length) continue;
                var cells = Array.from(rows[rowIndex].querySelectorAll('th,td')).filter(this._isVisible.bind(this));
                if (colIndex >= cells.length) continue;
                var cell = cells[colIndex];
                var target = cell;
                if (wantLink) target = cell.querySelector('a,[role="link"]') || cell;
                if (wantButton) target = cell.querySelector('button,[role="button"],input[type="button"],input[type="submit"]') || cell;
                this._pushCandidate(candidates, seen, target, 118, 'table cell');
            }
        },

        _findNaturalCandidates: function(query, options, candidates, seen) {
            var elements = this._candidateElements(options);
            for (var i = 0; i < elements.length; i++) {
                var score = this._rankElement(elements[i], query, options, 'natural');
                if (score > 0) {
                    this._pushCandidate(candidates, seen, elements[i], score, 'smart match');
                }
            }
        },

        _resolveCandidates: function(query, candidates, options) {
            options = options || {};
            candidates.sort(function(a, b) { return b.score - a.score; });
            var minScore = options.minScore || 55;
            candidates = candidates.filter(function(c) { return c.score >= minScore; }).slice(0, 12);
            var described = [];
            for (var i = 0; i < candidates.length; i++) {
                described.push(this._describeCandidate(candidates[i].el, candidates[i].score, candidates[i].reason));
            }
            if (candidates.length === 0) {
                return { ok: false, error: '元素未找到: ' + query, candidates: [] };
            }
            if (candidates.length > 1) {
                var gap = candidates[0].score - candidates[1].score;
                if (options.strictMultiple || gap < 25) {
                    return {
                        ok: false,
                        ambiguous: true,
                        error: '找到多个候选元素，请选择更精确目标',
                        candidates: described
                    };
                }
            }
            return {
                ok: true,
                element: candidates[0].el,
                selector: described[0].selector,
                target: described[0],
                candidates: []
            };
        },

        _locateElement: function(query, options) {
            options = options || {};
            query = (query || '').trim();
            if (!query) return { ok: false, error: '定位描述不能为空', candidates: [] };
            var candidates = [];
            var seen = [];

            var cssMatches = this._queryAll(query);
            if (cssMatches && cssMatches.length > 0) {
                for (var i = 0; i < cssMatches.length; i++) {
                    this._pushCandidate(candidates, seen, cssMatches[i], 120, 'css selector');
                }
                return this._resolveCandidates(query, candidates, { strictMultiple: candidates.length > 1, minScore: 1 });
            }

            this._findExplicitCandidates(query, options, candidates, seen);
            this._findTableCellTarget(query, options, candidates, seen);
            this._findNaturalCandidates(query, options, candidates, seen);
            return this._resolveCandidates(query, candidates, {});
        },

        locateElement: function(query, options) {
            var result = this._locateElement(query, options || {});
            if (!result.ok) {
                return {
                    ok: false,
                    ambiguous: !!result.ambiguous,
                    error: result.error,
                    candidates: result.candidates || []
                };
            }
            return {
                ok: true,
                selector: result.selector,
                target: result.target,
                candidates: []
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
                        var lbl = container.querySelector('label[for="' + this._escapeCssString(el.id) + '"]');
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
                    // 构造 selector 和 description
                    field.selector = this.generateSelector(el);
                    field.description = this._fieldDescription(field);
                    fields.push(field);
                }
                // 提取表单按钮
                var buttons = [];
                var btnEls = container.querySelectorAll('button,input[type="submit"],input[type="button"],input[type="reset"]');
                for (var bi = 0; bi < btnEls.length; bi++) {
                    var btn = btnEls[bi];
                    var btnText = (btn.textContent || btn.value || '').trim();
                    buttons.push({
                        type: btn.type || 'button',
                        text: btnText,
                        selector: this.generateSelector(btn),
                        description: btnText ? 'text=' + btnText : this.generateSelector(btn),
                        disabled: btn.disabled || false
                    });
                }
                if (fields.length > 0 || buttons.length > 0) {
                    forms.push({ fields: fields, buttons: buttons });
                }
            }
            return { forms: forms, framework: this.detectFramework(), uiComponents: this._extractUIComponents() };
        },

        _fieldDescription: function(field) {
            if (field.label) return 'label=' + field.label;
            if (field.placeholder) return 'placeholder=' + field.placeholder;
            if (field.name) return 'name=' + field.name;
            return field.selector || '';
        },

        extractInteractiveElements: function() {
            var result = [];
            var sel = 'button,a,input[type="button"],input[type="submit"],input[type="reset"],[role="button"],[role="link"],[role="tab"],[role="menuitem"],[role="treeitem"],[role="listitem"],summary,details';
            var els = this._queryAll(sel) || [];
            for (var i = 0; i < els.length && result.length < 50; i++) {
                var el = els[i];
                if (!this._isVisible(el)) continue;
                var text = (el.textContent || el.value || '').trim();
                if (text.length > 80) text = text.substring(0, 80) + '...';
                var tag = el.tagName.toLowerCase();
                var role = el.getAttribute('role') || '';
                var href = tag === 'a' ? (el.href || '') : '';
                var item = {
                    tag: tag,
                    text: text,
                    role: role,
                    selector: this.generateSelector(el),
                    description: text ? 'text=' + text : this.generateSelector(el),
                    disabled: el.disabled || false
                };
                if (href) item.href = href;
                result.push(item);
            }
            return { elements: result, count: result.length };
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
                    selector: this.generateSelector(sel),
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
                    selector: this.generateSelector(picker),
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
                    selector: this.generateSelector(es),
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
                    selector: this.generateSelector(ed),
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
            var located = this._locateElement(selector, { action: 'fill', components: false });
            if (!located.ok) {
                return {
                    ok: false,
                    error: located.error || ('元素未找到: ' + selector),
                    candidates: located.candidates || []
                };
            }
            var el = located.element;
            var locatedSelector = located.selector;
            var target = located.target;

            if (el.getAttribute && el.getAttribute('contenteditable') === 'true') {
                el.focus();
                el.textContent = value;
                el.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText', data: value }));
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return { ok: true, strategy: 'contenteditable', selector: locatedSelector, target: target, currentValue: el.textContent || '' };
            }

            // select 特殊处理
            if (el.tagName === 'SELECT') {
                el.value = value;
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return { ok: true, strategy: 'select-change', selector: locatedSelector, target: target, currentValue: el.value };
            }

            // checkbox / radio 特殊处理
            if (el.type === 'checkbox' || el.type === 'radio') {
                var shouldCheck = (value === 'true' || value === '1');
                if (el.checked !== shouldCheck) {
                    el.click();
                }
                return { ok: true, strategy: 'click-toggle', selector: locatedSelector, target: target, currentValue: String(el.checked) };
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
                    return { ok: true, strategy: 'keyboard', selector: locatedSelector, target: target, currentValue: el.value };
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
                        return { ok: true, strategy: 'native-setter', selector: locatedSelector, target: target, currentValue: el.value };
                    }
                }
            }

            // 策略 3: 粘贴（兜底）
            if (strategy === 'auto' || strategy === 'paste') {
                el.focus();
                el.value = value;
                el.dispatchEvent(new Event('input', { bubbles: true }));
                el.dispatchEvent(new Event('change', { bubbles: true }));
                return { ok: true, strategy: 'paste', selector: locatedSelector, target: target, currentValue: el.value };
            }

            return { ok: false, error: '所有填写策略均未成功', currentValue: el.value, selector: locatedSelector, target: target };
        },

        // UI 库组件填写（多步交互，异步返回）
        fillComponent: function(selector, value) {
            var located = this._locateElement(selector, { action: 'fill', components: true, componentOnly: true });
            if (!located.ok) {
                return {
                    ok: false,
                    error: located.error || ('组件未找到: ' + selector),
                    candidates: located.candidates || []
                };
            }
            var el = located.element;
            var result;

            // Ant Design Select
            if (el.classList.contains('ant-select')) {
                result = this._fillAntSelect(el, value);
            }
            // Ant Design DatePicker
            else if (el.classList.contains('ant-picker')) {
                result = this._fillAntDatePicker(el, value);
            }
            // Element Plus Select
            else if (el.classList.contains('el-select')) {
                result = this._fillElSelect(el, value);
            }
            // Element Plus DatePicker
            else if (el.classList.contains('el-date-editor')) {
                result = this._fillElDatePicker(el, value);
            } else {
                result = { ok: false, error: '未知的 UI 库组件类型' };
            }
            if (result) {
                result.selector = located.selector;
                result.target = located.target;
            }
            return result;
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

        clickElement: function(selector) {
            var located = this._locateElement(selector, { action: 'click' });
            if (!located.ok) {
                return {
                    ok: false,
                    error: located.error || ('元素未找到: ' + selector),
                    candidates: located.candidates || []
                };
            }
            var el = located.element;
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
            return {
                ok: true,
                selector: located.selector,
                target: located.target,
                candidates: [],
                x: Math.round(x),
                y: Math.round(y)
            };
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
            _selectedIndex: -1,
            _dragging: false,
            _dragOffsetX: 0,
            _dragOffsetY: 0,
            _drawing: false,
            _resizing: false,
            _resizeHandle: -1,
            _hoveredHandle: -1,

            start: function(tool) {
                if (this._active) return { ok: true, note: 'already active' };
                this._currentTool = tool || 'rect';
                this._active = true;
                this._annotations = [];
                this._selectedIndex = -1;
                this._ensureCanvas();
                this._bindEvents();
                this._showToolbar();
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
                this._hideToolbar();
                return { ok: true };
            },

            clear: function() {
                this._annotations = [];
                this._selectedIndex = -1;
                this._render();
                return { ok: true };
            },

            getAnnotations: function() {
                var self = this;
                var annotations = this._annotations.map(function(annotation, index) {
                    return self._enrichAnnotation(annotation, index);
                });
                return {
                    annotations: annotations,
                    count: annotations.length,
                    summary: this._formatAnnotationsForAgent(annotations)
                };
            },

            _bridge: function() {
                return window.__tiangong_bridge || {};
            },

            _annotationRect: function(annotation) {
                if (!annotation) return null;
                if (annotation.type === 'rect') {
                    return {
                        x: annotation.x,
                        y: annotation.y,
                        width: annotation.width,
                        height: annotation.height
                    };
                }
                if (annotation.type === 'arrow') {
                    var minX = Math.min(annotation.x1, annotation.x2);
                    var minY = Math.min(annotation.y1, annotation.y2);
                    var maxX = Math.max(annotation.x1, annotation.x2);
                    var maxY = Math.max(annotation.y1, annotation.y2);
                    var padding = 16;
                    return {
                        x: Math.max(0, minX - padding),
                        y: Math.max(0, minY - padding),
                        width: Math.max(1, maxX - minX + padding * 2),
                        height: Math.max(1, maxY - minY + padding * 2)
                    };
                }
                return null;
            },

            _intersectArea: function(a, b) {
                if (!a || !b) return 0;
                var left = Math.max(a.x, b.x);
                var top = Math.max(a.y, b.y);
                var right = Math.min(a.x + a.width, b.x + b.width);
                var bottom = Math.min(a.y + a.height, b.y + b.height);
                if (right <= left || bottom <= top) return 0;
                return (right - left) * (bottom - top);
            },

            _elementRect: function(el) {
                if (!el || !el.getBoundingClientRect) return null;
                var rect = el.getBoundingClientRect();
                if (!rect || rect.width <= 0 || rect.height <= 0) return null;
                return {
                    x: rect.left,
                    y: rect.top,
                    width: rect.width,
                    height: rect.height
                };
            },

            _rectKey: function(rect) {
                if (!rect) return '';
                return [
                    Math.round(rect.x),
                    Math.round(rect.y),
                    Math.round(rect.width),
                    Math.round(rect.height)
                ].join(',');
            },

            _normalizeText: function(text) {
                var bridge = this._bridge();
                if (bridge._normalizeText) return bridge._normalizeText(text);
                return (text || '').replace(/\s+/g, ' ').trim().toLowerCase();
            },

            _shortText: function(text, maxLen) {
                var bridge = this._bridge();
                if (bridge._shortText) return bridge._shortText(text, maxLen || 200);
                text = (text || '').replace(/\s+/g, ' ').trim();
                if (text.length > (maxLen || 200)) return text.substring(0, (maxLen || 200) - 1) + '…';
                return text;
            },

            _isElementVisible: function(el) {
                var bridge = this._bridge();
                if (bridge._isVisible) return bridge._isVisible(el);
                if (!el || !el.getBoundingClientRect) return false;
                var rect = el.getBoundingClientRect();
                return rect.width > 0 && rect.height > 0;
            },

            _extractTextInRect: function(regionRect) {
                var texts = [];
                var seen = {};
                if (!document.body || !document.createTreeWalker || !document.createRange || !window.NodeFilter) {
                    return '';
                }
                var walker = document.createTreeWalker(document.body, window.NodeFilter.SHOW_TEXT, {
                    acceptNode: function(node) {
                        var text = (node.nodeValue || '').replace(/\s+/g, ' ').trim();
                        if (!text) return window.NodeFilter.FILTER_REJECT;
                        var parent = node.parentElement;
                        if (!parent || parent.id === '__tiangong_annotation_canvas') return window.NodeFilter.FILTER_REJECT;
                        var tag = parent.tagName ? parent.tagName.toLowerCase() : '';
                        if (['script', 'style', 'noscript', 'template'].indexOf(tag) >= 0) return window.NodeFilter.FILTER_REJECT;
                        return window.NodeFilter.FILTER_ACCEPT;
                    }
                });
                var node;
                while ((node = walker.nextNode())) {
                    var range = document.createRange();
                    try {
                        range.selectNodeContents(node);
                        var rects = range.getClientRects ? Array.from(range.getClientRects()) : [];
                        for (var i = 0; i < rects.length; i++) {
                            var rect = {
                                x: rects[i].left,
                                y: rects[i].top,
                                width: rects[i].width,
                                height: rects[i].height
                            };
                            if (this._intersectArea(regionRect, rect) <= 0) continue;
                            var value = this._shortText(node.nodeValue || '', 180);
                            var key = this._normalizeText(value);
                            if (key && !seen[key]) {
                                seen[key] = true;
                                texts.push(value);
                            }
                            break;
                        }
                    } catch(e) {
                    } finally {
                        if (range.detach) range.detach();
                    }
                    if (texts.join(' ').length > 1200) break;
                }
                return this._shortText(texts.join(' '), 1200);
            },

            _elementText: function(el) {
                var bridge = this._bridge();
                var label = bridge._accessibleName ? bridge._accessibleName(el) : '';
                var text = bridge._visibleText ? bridge._visibleText(el) : (el ? (el.innerText || el.textContent || '') : '');
                var value = '';
                if (el && (el.value || el.placeholder)) {
                    value = el.value || el.placeholder || '';
                }
                return this._shortText(label || text || value, 160);
            },

            _elementSummary: function(el, overlap) {
                var bridge = this._bridge();
                var rect = this._elementRect(el);
                return {
                    selector: bridge.generateSelector ? bridge.generateSelector(el) : '',
                    tag: el && el.tagName ? el.tagName.toLowerCase() : '',
                    role: (el && (el.getAttribute('role') || (bridge._implicitRole ? bridge._implicitRole(el) : ''))) || '',
                    text: this._elementText(el),
                    overlap: Math.round(overlap),
                    x: rect ? Math.round(rect.x + rect.width / 2) : null,
                    y: rect ? Math.round(rect.y + rect.height / 2) : null
                };
            },

            _extractElementsInRect: function(regionRect) {
                var selector = [
                    'button',
                    'a',
                    'input',
                    'textarea',
                    'select',
                    'label',
                    'h1',
                    'h2',
                    'h3',
                    'h4',
                    'h5',
                    'h6',
                    'p',
                    'li',
                    'td',
                    'th',
                    'summary',
                    '[role]',
                    '[aria-label]',
                    '[title]',
                    '[placeholder]',
                    'img'
                ].join(',');
                var elements = Array.from(document.querySelectorAll(selector));
                var matches = [];
                for (var i = 0; i < elements.length; i++) {
                    var el = elements[i];
                    if (!el || el.id === '__tiangong_annotation_canvas' || !this._isElementVisible(el)) continue;
                    var rect = this._elementRect(el);
                    if (!rect) continue;
                    var overlap = this._intersectArea(regionRect, rect);
                    if (overlap <= 0) continue;
                    var elementArea = rect.width * rect.height;
                    var regionArea = regionRect.width * regionRect.height;
                    var centerInside = rect.x + rect.width / 2 >= regionRect.x &&
                        rect.x + rect.width / 2 <= regionRect.x + regionRect.width &&
                        rect.y + rect.height / 2 >= regionRect.y &&
                        rect.y + rect.height / 2 <= regionRect.y + regionRect.height;
                    if (!centerInside && overlap / elementArea < 0.18 && overlap / regionArea < 0.08) continue;
                    var text = this._elementText(el);
                    if (!text && !el.getAttribute('aria-label') && !el.getAttribute('title') && !el.getAttribute('placeholder')) continue;
                    matches.push({ el: el, rect: rect, overlap: overlap });
                }
                matches.sort(function(a, b) {
                    if (Math.abs(a.rect.y - b.rect.y) > 4) return a.rect.y - b.rect.y;
                    if (Math.abs(a.rect.x - b.rect.x) > 4) return a.rect.x - b.rect.x;
                    return b.overlap - a.overlap;
                });

                var result = [];
                var seenText = {};
                for (var j = 0; j < matches.length; j++) {
                    var summary = this._elementSummary(matches[j].el, matches[j].overlap);
                    var key = this._normalizeText(summary.tag + ' ' + summary.text + ' ' + summary.selector);
                    if (!key || seenText[key]) continue;
                    seenText[key] = true;
                    result.push(summary);
                    if (result.length >= 10) break;
                }
                return result;
            },

            _fallbackElementAtCenter: function(regionRect) {
                if (!document.elementFromPoint) return [];
                var x = regionRect.x + regionRect.width / 2;
                var y = regionRect.y + regionRect.height / 2;
                var el = document.elementFromPoint(x, y);
                if (!el || el.id === '__tiangong_annotation_canvas') return [];
                while (el && el !== document.body && !this._elementText(el)) {
                    el = el.parentElement;
                }
                if (!el || el === document.body || !this._isElementVisible(el)) return [];
                return [this._elementSummary(el, 0)];
            },

            _extractRegion: function(annotation) {
                var rect = this._annotationRect(annotation);
                if (!rect || rect.width <= 0 || rect.height <= 0) {
                    return { text: '', elements: [], elementCount: 0 };
                }
                // 将页面坐标转为视口坐标用于文本/元素提取
                var vp = this._toViewport(rect.x, rect.y);
                var viewportRect = { x: vp.x, y: vp.y, width: rect.width, height: rect.height };
                var text = this._extractTextInRect(viewportRect);
                var elements = this._extractElementsInRect(viewportRect);
                if (elements.length === 0) {
                    elements = this._fallbackElementAtCenter(viewportRect);
                }
                if (!text && elements.length > 0) {
                    text = this._shortText(elements.map(function(el) { return el.text; }).filter(Boolean).join(' '), 1200);
                }
                return {
                    x: Math.round(rect.x),
                    y: Math.round(rect.y),
                    width: Math.round(rect.width),
                    height: Math.round(rect.height),
                    text: text,
                    elements: elements,
                    elementCount: elements.length
                };
            },

            _enrichAnnotation: function(annotation, index) {
                var enriched = {};
                for (var key in annotation) {
                    if (Object.prototype.hasOwnProperty.call(annotation, key)) {
                        enriched[key] = annotation[key];
                    }
                }
                enriched.index = index + 1;
                enriched.region = this._extractRegion(annotation);
                return enriched;
            },

            _formatAnnotationsForAgent: function(annotations) {
                if (!annotations || annotations.length === 0) return '';
                var lines = ['[页面批注]'];
                for (var i = 0; i < annotations.length; i++) {
                    var annotation = annotations[i];
                    var region = annotation.region || {};
                    lines.push((i + 1) + '. ' + (annotation.type === 'arrow' ? '箭头批注' : '矩形批注') +
                        ' 区域：x=' + (region.x || 0) +
                        ', y=' + (region.y || 0) +
                        ', w=' + (region.width || 0) +
                        ', h=' + (region.height || 0));
                    if (region.text) {
                        lines.push('   区域文本：' + this._shortText(region.text, 600));
                    }
                    if (region.elements && region.elements.length > 0) {
                        lines.push('   覆盖元素：');
                        for (var j = 0; j < region.elements.length && j < 5; j++) {
                            var el = region.elements[j];
                            var identity = [el.tag, el.role ? 'role=' + el.role : '', el.text ? 'text="' + this._shortText(el.text, 80) + '"' : '']
                                .filter(Boolean)
                                .join(' ');
                            lines.push('   - ' + identity + (el.selector ? ' | selector: ' + el.selector : ''));
                        }
                    }
                    if (!region.text && (!region.elements || region.elements.length === 0)) {
                        lines.push('   未提取到明显文本或元素');
                    }
                }
                return lines.join('\n');
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
                this._scrollHandler = function() { self._render(); };
                window.addEventListener('resize', this._resizeHandler);
                window.addEventListener('scroll', this._scrollHandler, true);
                this._resize();
            },

            _resize: function() {
                if (!this._canvas) return;
                this._canvas.width = window.innerWidth;
                this._canvas.height = window.innerHeight;
                this._render();
            },

            // 将页面坐标转换为视口坐标
            _toViewport: function(px, py) {
                return { x: px - window.scrollX, y: py - window.scrollY };
            },

            // 将视口坐标转换为页面坐标
            _toPage: function(vx, vy) {
                return { x: vx + window.scrollX, y: vy + window.scrollY };
            },

            // 判断视口坐标是否命中某个批注
            _hitTest: function(vx, vy, annotation) {
                if (annotation.type === 'rect') {
                    var vp = this._toViewport(annotation.x, annotation.y);
                    // 矩形内部全部可选中，外围扩展 6px
                    return vx >= vp.x - 6 && vx <= vp.x + annotation.width + 6 &&
                           vy >= vp.y - 6 && vy <= vp.y + annotation.height + 6;
                }
                if (annotation.type === 'arrow') {
                    var a = this._toViewport(annotation.x1, annotation.y1);
                    var b = this._toViewport(annotation.x2, annotation.y2);
                    // 箭头线段容差 14px，端点单独检测（半径 18px）
                    if (this._pointDist(vx, vy, a.x, a.y) < 18) return true;
                    if (this._pointDist(vx, vy, b.x, b.y) < 18) return true;
                    return this._pointToSegmentDist(vx, vy, a.x, a.y, b.x, b.y) < 14;
                }
                return false;
            },

            _pointDist: function(px, py, x, y) {
                var dx = px - x, dy = py - y;
                return Math.sqrt(dx * dx + dy * dy);
            },

            _pointToSegmentDist: function(px, py, x1, y1, x2, y2) {
                var dx = x2 - x1, dy = y2 - y1;
                var len2 = dx * dx + dy * dy;
                if (len2 === 0) return Math.sqrt((px - x1) * (px - x1) + (py - y1) * (py - y1));
                var t = Math.max(0, Math.min(1, ((px - x1) * dx + (py - y1) * dy) / len2));
                var cx = x1 + t * dx, cy = y1 + t * dy;
                return Math.sqrt((px - cx) * (px - cx) + (py - cy) * (py - cy));
            },

            _bindEvents: function() {
                var self = this;
                this._onDown = function(e) { self._handleDown(e); };
                this._onMove = function(e) { self._handleMove(e); };
                this._onUp = function(e) { self._handleUp(e); };
                this._onKeyDown = function(e) { self._handleKeyDown(e); };
                this._canvas.addEventListener('mousedown', this._onDown);
                this._canvas.addEventListener('mousemove', this._onMove);
                this._canvas.addEventListener('mouseup', this._onUp);
                document.addEventListener('keydown', this._onKeyDown);
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
                if (this._scrollHandler) {
                    window.removeEventListener('scroll', this._scrollHandler, true);
                }
                if (this._onKeyDown) {
                    document.removeEventListener('keydown', this._onKeyDown);
                }
            },

            _handleKeyDown: function(e) {
                if (this._selectedIndex < 0) return;
                if (e.key === 'Delete' || e.key === 'Backspace') {
                    this._annotations.splice(this._selectedIndex, 1);
                    this._selectedIndex = -1;
                    this._render();
                } else if (e.key === 'Escape') {
                    this._selectedIndex = -1;
                    this._render();
                }
            },

            _handleDown: function(e) {
                var vx = e.clientX, vy = e.clientY;

                // 1. 如果已有选中批注，先检测是否点击了控制点
                if (this._selectedIndex >= 0 && this._selectedIndex < this._annotations.length) {
                    var sel = this._annotations[this._selectedIndex];
                    var handles = sel.type === 'rect' ? this._getRectHandles(sel) : this._getArrowHandles(sel);
                    for (var h = 0; h < handles.length; h++) {
                        if (Math.abs(vx - handles[h].x) < 10 && Math.abs(vy - handles[h].y) < 10) {
                            this._resizing = true;
                            this._resizeHandle = h;
                            this._dragging = false;
                            this._drawing = false;
                            return;
                        }
                    }
                }

                // 2. 检测是否点击了已有批注（不含控制点）
                for (var i = this._annotations.length - 1; i >= 0; i--) {
                    if (this._hitTest(vx, vy, this._annotations[i])) {
                        this._selectedIndex = i;
                        this._dragging = true;
                        this._drawing = false;
                        this._resizing = false;
                        var a = this._annotations[i];
                        if (a.type === 'rect') {
                            var vp = this._toViewport(a.x, a.y);
                            this._dragOffsetX = vx - vp.x;
                            this._dragOffsetY = vy - vp.y;
                        } else {
                            this._dragOffsetX = vx;
                            this._dragOffsetY = vy;
                        }
                        this._render();
                        return;
                    }
                }

                // 3. 未命中任何批注，开始新绘制
                this._selectedIndex = -1;
                this._startX = vx;
                this._startY = vy;
                this._drawing = true;
                this._dragging = false;
                this._resizing = false;
            },

            _handleMove: function(e) {
                var vx = e.clientX, vy = e.clientY;

                // 更新光标样式
                this._updateCursor(vx, vy);

                // 控制点拖拽调整大小
                if (this._resizing && this._selectedIndex >= 0) {
                    var a = this._annotations[this._selectedIndex];
                    var pg = this._toPage(vx, vy);
                    if (a.type === 'rect') {
                        this._resizeRect(a, this._resizeHandle, pg.x, pg.y);
                    } else if (a.type === 'arrow') {
                        if (this._resizeHandle === 0) {
                            a.x1 = pg.x; a.y1 = pg.y;
                        } else {
                            a.x2 = pg.x; a.y2 = pg.y;
                        }
                    }
                    this._render();
                    return;
                }

                // 拖拽移动已选中的批注
                if (this._dragging && this._selectedIndex >= 0) {
                    var a = this._annotations[this._selectedIndex];
                    var pg;
                    if (a.type === 'rect') {
                        pg = this._toPage(vx - this._dragOffsetX, vy - this._dragOffsetY);
                        a.x = pg.x;
                        a.y = pg.y;
                    } else {
                        var dx = vx - this._dragOffsetX;
                        var dy = vy - this._dragOffsetY;
                        pg = this._toPage(a.x1 + dx, a.y1 + dy);
                        var pg2 = this._toPage(a.x2 + dx, a.y2 + dy);
                        a.x1 = pg.x; a.y1 = pg.y;
                        a.x2 = pg2.x; a.y2 = pg2.y;
                        this._dragOffsetX = vx;
                        this._dragOffsetY = vy;
                    }
                    this._render();
                    return;
                }

                // 绘制新批注预览
                if (!this._drawing) return;
                this._render();
                var ctx = this._ctx;
                ctx.strokeStyle = this._color;
                ctx.lineWidth = this._strokeWidth;
                ctx.setLineDash([6, 4]);
                if (this._currentTool === 'rect') {
                    ctx.strokeRect(this._startX, this._startY, vx - this._startX, vy - this._startY);
                } else if (this._currentTool === 'arrow') {
                    this._drawArrow(ctx, this._startX, this._startY, vx, vy);
                }
                ctx.setLineDash([]);
            },

            _updateCursor: function(vx, vy) {
                if (!this._canvas) return;
                var prevHover = this._hoveredHandle;
                this._hoveredHandle = -1;
                // 1. 检测控制点悬停
                if (this._selectedIndex >= 0 && this._selectedIndex < this._annotations.length) {
                    var sel = this._annotations[this._selectedIndex];
                    var handles = sel.type === 'rect' ? this._getRectHandles(sel) : this._getArrowHandles(sel);
                    for (var h = 0; h < handles.length; h++) {
                        if (Math.abs(vx - handles[h].x) < 10 && Math.abs(vy - handles[h].y) < 10) {
                            this._hoveredHandle = h;
                            if (sel.type === 'rect') {
                                var cursors = ['nwse-resize','ns-resize','nesw-resize','ew-resize','nwse-resize','ns-resize','nesw-resize','ew-resize'];
                                this._canvas.style.cursor = cursors[h];
                            } else {
                                this._canvas.style.cursor = 'grab';
                            }
                            if (prevHover !== h) this._render();
                            return;
                        }
                    }
                }
                // 2. 检测批注悬停
                for (var i = this._annotations.length - 1; i >= 0; i--) {
                    if (this._hitTest(vx, vy, this._annotations[i])) {
                        this._canvas.style.cursor = 'move';
                        if (prevHover !== -1) this._render();
                        return;
                    }
                }
                this._canvas.style.cursor = 'crosshair';
                if (prevHover !== -1) this._render();
            },

            _resizeRect: function(a, handle, px, py) {
                var right = a.x + a.width;
                var bottom = a.y + a.height;
                switch (handle) {
                    case 0: a.x = px; a.y = py; a.width = right - px; a.height = bottom - py; break;
                    case 1: a.y = py; a.height = bottom - py; break;
                    case 2: a.width = px - a.x; a.y = py; a.height = bottom - py; break;
                    case 3: a.width = px - a.x; break;
                    case 4: a.width = px - a.x; a.height = py - a.y; break;
                    case 5: a.height = py - a.y; break;
                    case 6: a.x = px; a.width = right - px; a.height = py - a.y; break;
                    case 7: a.x = px; a.width = right - px; break;
                }
                // 防止翻转（宽高为负时修正）
                if (a.width < 0) { a.x += a.width; a.width = -a.width; }
                if (a.height < 0) { a.y += a.height; a.height = -a.height; }
            },

            _handleUp: function(e) {
                // 完成控制点拖拽
                if (this._resizing) {
                    this._resizing = false;
                    this._resizeHandle = -1;
                    return;
                }
                // 完成拖拽移动
                if (this._dragging) {
                    this._dragging = false;
                    return;
                }
                if (!this._drawing) return;
                this._drawing = false;
                var vx = e.clientX, vy = e.clientY;
                var pg = this._toPage(this._startX, this._startY);
                var pg2 = this._toPage(vx, vy);
                var annotation = {
                    type: this._currentTool,
                    color: this._color
                };
                if (this._currentTool === 'rect') {
                    annotation.x = Math.min(pg.x, pg2.x);
                    annotation.y = Math.min(pg.y, pg2.y);
                    annotation.width = Math.abs(pg2.x - pg.x);
                    annotation.height = Math.abs(pg2.y - pg.y);
                } else if (this._currentTool === 'arrow') {
                    annotation.x1 = pg.x;
                    annotation.y1 = pg.y;
                    annotation.x2 = pg2.x;
                    annotation.y2 = pg2.y;
                }
                if (annotation.width > 5 || annotation.height > 5 || annotation.x1 !== undefined) {
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

            // 获取矩形 8 个控制点的视口坐标（顺时针：TL, T, TR, R, BR, B, BL, L）
            _getRectHandles: function(a) {
                var vp = this._toViewport(a.x, a.y);
                var x = vp.x, y = vp.y, w = a.width, h = a.height;
                return [
                    { x: x, y: y },           // 0: TL
                    { x: x + w / 2, y: y },   // 1: T
                    { x: x + w, y: y },        // 2: TR
                    { x: x + w, y: y + h / 2 },// 3: R
                    { x: x + w, y: y + h },    // 4: BR
                    { x: x + w / 2, y: y + h },// 5: B
                    { x: x, y: y + h },         // 6: BL
                    { x: x, y: y + h / 2 }      // 7: L
                ];
            },

            // 获取箭头 2 个端点的视口坐标
            _getArrowHandles: function(a) {
                var va = this._toViewport(a.x1, a.y1);
                var vb = this._toViewport(a.x2, a.y2);
                return [{ x: va.x, y: va.y }, { x: vb.x, y: vb.y }];
            },

            _drawHandle: function(ctx, x, y, isHovered) {
                var r = isHovered ? 7 : 4;
                ctx.fillStyle = isHovered ? '#6366f1' : '#fff';
                ctx.strokeStyle = 'rgba(99,102,241,0.9)';
                ctx.lineWidth = isHovered ? 2 : 1.5;
                ctx.beginPath();
                ctx.arc(x, y, r, 0, Math.PI * 2);
                ctx.fill();
                ctx.stroke();
            },

            _render: function() {
                if (!this._ctx) return;
                this._ctx.clearRect(0, 0, this._canvas.width, this._canvas.height);
                for (var i = 0; i < this._annotations.length; i++) {
                    var a = this._annotations[i];
                    var isSelected = (i === this._selectedIndex);
                    this._ctx.strokeStyle = a.color;
                    this._ctx.lineWidth = 2;
                    this._ctx.setLineDash([]);
                    if (a.type === 'rect') {
                        var vp = this._toViewport(a.x, a.y);
                        this._ctx.strokeRect(vp.x, vp.y, a.width, a.height);
                        if (isSelected) {
                            var handles = this._getRectHandles(a);
                            for (var h = 0; h < handles.length; h++) {
                                this._drawHandle(this._ctx, handles[h].x, handles[h].y, h === this._hoveredHandle);
                            }
                        }
                    } else if (a.type === 'arrow') {
                        var va = this._toViewport(a.x1, a.y1);
                        var vb = this._toViewport(a.x2, a.y2);
                        this._drawArrow(this._ctx, va.x, va.y, vb.x, vb.y);
                        if (isSelected) {
                            var handles = this._getArrowHandles(a);
                            for (var h = 0; h < handles.length; h++) {
                                this._drawHandle(this._ctx, handles[h].x, handles[h].y, h === this._hoveredHandle);
                            }
                        }
                    }
                }
                this._updateToolbarCount();
            },

            _showToolbar: function() {
                if (this._toolbar) return;
                var self = this;
                var toolbar = document.createElement('div');
                toolbar.id = '__tiangong_annotation_toolbar';
                toolbar.style.cssText = 'position:fixed;bottom:16px;right:16px;z-index:2147483647;display:flex;align-items:center;gap:6px;padding:6px 10px;background:rgba(30,30,30,0.92);border-radius:8px;box-shadow:0 2px 12px rgba(0,0,0,0.3);font-family:system-ui,-apple-system,sans-serif;font-size:13px;color:#e5e5e5;user-select:none;backdrop-filter:blur(8px);';
                var tools = [
                    { key: 'rect', label: '矩形', icon: '▭' },
                    { key: 'arrow', label: '箭头', icon: '→' }
                ];
                for (var i = 0; i < tools.length; i++) {
                    (function(t) {
                        var btn = document.createElement('button');
                        btn.setAttribute('data-tool', t.key);
                        btn.textContent = t.icon + ' ' + t.label;
                        btn.title = t.label + '工具';
                        btn.style.cssText = 'padding:4px 10px;border:1px solid rgba(255,255,255,0.15);border-radius:5px;background:transparent;color:#e5e5e5;cursor:pointer;font-size:13px;line-height:1.2;transition:all 0.15s;';
                        if (self._currentTool === t.key) {
                            btn.style.background = 'rgba(99,102,241,0.8)';
                            btn.style.borderColor = 'rgba(99,102,241,0.9)';
                        }
                        btn.onmouseenter = function() { if (self._currentTool !== t.key) btn.style.background = 'rgba(255,255,255,0.1)'; };
                        btn.onmouseleave = function() { if (self._currentTool !== t.key) btn.style.background = 'transparent'; };
                        btn.onclick = function() { self._switchTool(t.key); };
                        toolbar.appendChild(btn);
                    })(tools[i]);
                }
                var sep = document.createElement('span');
                sep.style.cssText = 'width:1px;height:18px;background:rgba(255,255,255,0.15);margin:0 2px;';
                toolbar.appendChild(sep);
                var countSpan = document.createElement('span');
                countSpan.setAttribute('data-role', 'count');
                countSpan.style.cssText = 'font-size:12px;color:#9ca3af;min-width:32px;text-align:center;';
                countSpan.textContent = '0 个';
                toolbar.appendChild(countSpan);
                var clearBtn = document.createElement('button');
                clearBtn.setAttribute('data-role', 'clear');
                clearBtn.textContent = '清除';
                clearBtn.title = '清除所有批注';
                clearBtn.style.cssText = 'padding:4px 10px;border:1px solid rgba(239,68,68,0.3);border-radius:5px;background:transparent;color:#f87171;cursor:pointer;font-size:13px;line-height:1.2;transition:all 0.15s;';
                clearBtn.onmouseenter = function() { clearBtn.style.background = 'rgba(239,68,68,0.15)'; };
                clearBtn.onmouseleave = function() { clearBtn.style.background = 'transparent'; };
                clearBtn.onclick = function() { self.clear(); };
                toolbar.appendChild(clearBtn);
                var closeBtn = document.createElement('button');
                closeBtn.textContent = '✕';
                closeBtn.title = '关闭批注';
                closeBtn.style.cssText = 'padding:4px 8px;border:1px solid rgba(255,255,255,0.15);border-radius:5px;background:transparent;color:#9ca3af;cursor:pointer;font-size:13px;line-height:1.2;transition:all 0.15s;';
                closeBtn.onmouseenter = function() { closeBtn.style.background = 'rgba(255,255,255,0.1)'; };
                closeBtn.onmouseleave = function() { closeBtn.style.background = 'transparent'; };
                closeBtn.onclick = function() {
                    var bridge = window.__tiangong_bridge;
                    if (bridge && bridge.annotation) bridge.annotation.stop();
                };
                toolbar.appendChild(closeBtn);
                document.body.appendChild(toolbar);
                this._toolbar = toolbar;
            },

            _hideToolbar: function() {
                if (this._toolbar && this._toolbar.parentNode) {
                    this._toolbar.parentNode.removeChild(this._toolbar);
                }
                this._toolbar = null;
            },

            _switchTool: function(tool) {
                this._currentTool = tool;
                this._selectedIndex = -1;
                var buttons = this._toolbar.querySelectorAll('[data-tool]');
                for (var i = 0; i < buttons.length; i++) {
                    var btn = buttons[i];
                    if (btn.getAttribute('data-tool') === tool) {
                        btn.style.background = 'rgba(99,102,241,0.8)';
                        btn.style.borderColor = 'rgba(99,102,241,0.9)';
                    } else {
                        btn.style.background = 'transparent';
                        btn.style.borderColor = 'rgba(255,255,255,0.15)';
                    }
                }
                this._render();
            },

            _updateToolbarCount: function() {
                if (!this._toolbar) return;
                var span = this._toolbar.querySelector('[data-role="count"]');
                if (span) {
                    span.textContent = this._annotations.length + ' 个';
                }
            },
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

    console.log('[Tiangong Bridge] loaded v0.9.0');
})();
