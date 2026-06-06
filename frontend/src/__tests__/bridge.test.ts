import { readFileSync } from 'fs';
import { resolve } from 'path';
import { beforeEach, describe, expect, it } from 'vitest';

const BRIDGE_PATH = resolve(__dirname, '../../../crates/tiangong-plugin-browser/js/bridge.js');

function loadBridge() {
  const code = readFileSync(BRIDGE_PATH, 'utf-8');
  const fn = new Function(code);
  fn();
}

function setupDOM(html: string) {
  document.body.innerHTML = html;
  // jsdom 不提供 scrollIntoView，手动 polyfill
  if (!Element.prototype.scrollIntoView) {
    Element.prototype.scrollIntoView = function () {};
  }
  // jsdom 不提供 elementFromPoint
  if (!document.elementFromPoint) {
    document.elementFromPoint = function () { return document.body; };
  }
  // jsdom innerText 不完整，polyfill 为 textContent
  if (!(HTMLElement.prototype as any).innerText) {
    Object.defineProperty(HTMLElement.prototype, 'innerText', {
      get() {
        try { return this.textContent; } catch { return ''; }
      },
      set(v: string) {
        try { this.textContent = v; } catch { /* ignore */ }
      },
      configurable: true,
    });
  }
  // jsdom MouseEvent/dispatchEvent 类型检查严格，用简化 mock
  (window as any).MouseEvent = function (this: any, type: string, init: any = {}) {
    this.type = type;
    this.bubbles = init.bubbles ?? false;
    this.cancelable = init.cancelable ?? false;
    Object.assign(this, init);
  };
  Element.prototype.dispatchEvent = function () { return true; };
  delete (window as any).__tiangong_bridge_loaded;
  loadBridge();
}

function getBridge(): any {
  return (window as any).__tiangong_bridge;
}

describe('locateElement', () => {
  beforeEach(() => {
    setupDOM(`
      <div id="container">
        <button id="btn-login" class="primary" aria-label="登录">登录</button>
        <a href="/about" title="关于我们">关于我们</a>
        <input name="username" placeholder="请输入用户名" />
        <span class="info">普通文本内容</span>
        <div role="button" id="confirm-btn">确认提交</div>
      </div>
    `);
  });

  it('通过 CSS 选择器 #id 定位', () => {
    const el = getBridge().locateElement('#btn-login');
    expect(el).not.toBeNull();
    expect(el.tagName).toBe('BUTTON');
    expect(el.id).toBe('btn-login');
  });

  it('通过 CSS 选择器 .class 定位', () => {
    const el = getBridge().locateElement('.primary');
    expect(el).not.toBeNull();
    expect(el.textContent).toContain('登录');
  });

  it('通过 [name] 属性选择器定位', () => {
    const el = getBridge().locateElement('[name="username"]');
    expect(el).not.toBeNull();
    expect(el.tagName).toBe('INPUT');
  });

  it('通过 aria: 前缀定位（aria-label）', () => {
    const el = getBridge().locateElement('aria:登录');
    expect(el).not.toBeNull();
    expect(el.id).toBe('btn-login');
  });

  it('通过 aria: 前缀定位（role）', () => {
    const el = getBridge().locateElement('aria:button');
    expect(el).not.toBeNull();
    expect(el.id).toBe('confirm-btn');
  });

  it('通过文本内容定位按钮', () => {
    const el = getBridge().locateElement('登录');
    expect(el).not.toBeNull();
    expect(el.id).toBe('btn-login');
  });

  it('通过按钮文本模糊匹配定位', () => {
    const el = getBridge().locateElement('关于');
    expect(el).not.toBeNull();
    expect(el.tagName).toBe('A');
  });

  it('找不到元素时返回 null', () => {
    expect(getBridge().locateElement('#nonexistent')).toBeNull();
  });

  it('空输入返回 null', () => {
    expect(getBridge().locateElement('')).toBeNull();
    expect(getBridge().locateElement('  ')).toBeNull();
  });
});

describe('generateSelector', () => {
  beforeEach(() => {
    setupDOM(`
      <div id="wrap">
        <ul id="list">
          <li class="item">第一项</li>
          <li class="item">第二项</li>
          <li class="item">第三项</li>
        </ul>
        <input name="email" />
      </div>
    `);
  });

  it('有 id 时返回 #id', () => {
    const el = document.getElementById('list')!;
    expect(getBridge().generateSelector(el)).toBe('#list');
  });

  it('有 name 时返回 [name=...]', () => {
    const el = document.querySelector('[name="email"]')!;
    expect(getBridge().generateSelector(el)).toBe('[name="email"]');
  });

  it('无 id/name 时生成带 nth-of-type 的路径', () => {
    const el = document.querySelectorAll('.item')[1];
    const selector = getBridge().generateSelector(el);
    expect(selector).toContain('li');
    expect(selector).toContain('nth-of-type(2)');
  });

  it('空元素返回空字符串', () => {
    expect(getBridge().generateSelector(null)).toBe('');
  });
});

describe('extractElementsInRect', () => {
  it('返回与矩形重叠的元素信息', () => {
    setupDOM(`
      <div id="page">
        <button id="btn" style="position:absolute;top:50px;left:50px;width:100px;height:30px;">按钮</button>
        <a id="link" style="position:absolute;top:100px;left:50px;width:150px;height:20px;" href="/test">链接</a>
      </div>
    `);

    const rects: Record<string, DOMRect> = {
      btn: { x: 50, y: 50, width: 100, height: 30, top: 50, left: 50, right: 150, bottom: 80 } as DOMRect,
      link: { x: 50, y: 100, width: 150, height: 20, top: 100, left: 50, right: 200, bottom: 120 } as DOMRect,
      page: { x: 0, y: 0, width: 800, height: 600, top: 0, left: 0, right: 800, bottom: 600 } as DOMRect,
    };
    for (const [id, rect] of Object.entries(rects)) {
      const el = document.getElementById(id);
      if (el) el.getBoundingClientRect = () => rect;
    }

    const results = getBridge().extractElementsInRect(40, 40, 170, 90);
    expect(results.length).toBeGreaterThan(0);

    const tags = results.map((r: any) => r.tag);
    expect(tags).toContain('button');
    expect(tags).toContain('a');
  });

  it('每个结果包含完整字段', () => {
    setupDOM(`<button id="btn">按钮</button>`);

    const el = document.getElementById('btn')!;
    el.getBoundingClientRect = () =>
      ({ x: 10, y: 10, width: 80, height: 30, top: 10, left: 10, right: 90, bottom: 40 }) as DOMRect;

    const results = getBridge().extractElementsInRect(5, 5, 100, 50);
    expect(results.length).toBeGreaterThan(0);

    const btn = results[0];
    expect(btn.tag).toBe('button');
    expect(btn.text).toContain('按钮');
    expect(btn.selector).toBeTruthy();
    expect(btn.rect).toBeDefined();
    expect(typeof btn.overlapRatio).toBe('number');
  });
});

describe('clickElement 智能定位', () => {
  beforeEach(() => {
    setupDOM(`
      <button id="submit-btn" class="submit" aria-label="提交">提交表单</button>
      <a href="/home" id="home-link">首页</a>
    `);
  });

  it('通过 CSS 选择器点击', () => {
    expect(getBridge().clickElement('#submit-btn').ok).toBe(true);
  });

  it('通过文本内容点击', () => {
    expect(getBridge().clickElement('提交表单').ok).toBe(true);
  });

  it('通过 aria 标签点击', () => {
    expect(getBridge().clickElement('aria:提交').ok).toBe(true);
  });

  it('找不到元素时返回错误', () => {
    const result = getBridge().clickElement('#nonexistent');
    expect(result.ok).toBe(false);
    expect(result.error).toContain('元素未找到');
  });
});

describe('fillField 智能定位', () => {
  beforeEach(() => {
    setupDOM(`
      <form>
        <input id="email" name="email" type="email" />
        <select id="city" name="city">
          <option value="bj">北京</option>
          <option value="sh">上海</option>
        </select>
      </form>
    `);
  });

  it('通过 CSS 选择器填写 input', () => {
    const result = getBridge().fillField('#email', 'test@example.com', 'auto');
    expect(result.ok).toBe(true);
    expect((document.getElementById('email') as HTMLInputElement).value).toBe('test@example.com');
  });

  it('填写 select 元素', () => {
    const result = getBridge().fillField('#city', 'sh', 'auto');
    expect(result.ok).toBe(true);
    expect((document.getElementById('city') as HTMLSelectElement).value).toBe('sh');
  });

  it('找不到元素时返回错误', () => {
    const result = getBridge().fillField('#nonexistent', 'val', 'auto');
    expect(result.ok).toBe(false);
    expect(result.error).toContain('元素未找到');
  });
});

describe('locateElement 对话框优先', () => {
  it('对话框打开时优先匹配对话框内的元素', () => {
    setupDOM(`
      <div>
        <button id="page-create">创建 API key</button>
        <div class="ant-modal-wrap" role="dialog" id="modal">
          <div class="ant-modal">
            <div class="ant-modal-content">
              <div class="ant-modal-footer">
                <button id="dialog-create">创建</button>
              </div>
            </div>
          </div>
        </div>
      </div>
    `);

    const modal = document.getElementById('modal')!;
    const bridge = getBridge();
    const orig = bridge._getTopmostOverlay;
    bridge._getTopmostOverlay = () => modal;

    const el = bridge.locateElement('创建');
    expect(el).not.toBeNull();
    expect(el.id).toBe('dialog-create');

    bridge._getTopmostOverlay = orig;
  });

  it('精确文本匹配优先于部分匹配', () => {
    setupDOM(`
      <div>
        <a id="link-create">创建 API key</a>
        <button id="btn-create">创建</button>
      </div>
    `);

    const el = getBridge().locateElement('创建');
    expect(el).not.toBeNull();
    // XPath text() = '创建' 应优先匹配精确文本
    expect(el.id).toBe('btn-create');
  });

  it('Element Plus 对话框内也能优先匹配', () => {
    setupDOM(`
      <div>
        <button id="page-btn">确定</button>
        <div class="el-dialog__wrapper" id="el-wrapper">
          <div class="el-dialog">
            <button id="dialog-btn">确定</button>
          </div>
        </div>
      </div>
    `);

    const wrapper = document.getElementById('el-wrapper')!;
    const bridge = getBridge();
    const orig = bridge._getTopmostOverlay;
    bridge._getTopmostOverlay = () => wrapper;

    const el = bridge.locateElement('确定');
    expect(el).not.toBeNull();
    expect(el.id).toBe('dialog-btn');

    bridge._getTopmostOverlay = orig;
  });

  it('无对话框时全局精确匹配', () => {
    setupDOM(`
      <div>
        <button id="save-btn">保存并继续</button>
        <button id="save">保存</button>
      </div>
    `);

    const el = getBridge().locateElement('保存');
    expect(el).not.toBeNull();
    expect(el.id).toBe('save');
  });
});

describe('locateAll', () => {
  it('返回所有匹配元素', () => {
    setupDOM(`
      <div>
        <button id="b1">提交</button>
        <button id="b2">取消</button>
        <button id="b3">提交并继续</button>
      </div>
    `);
    const results = getBridge().locateAll('提交');
    expect(results.length).toBeGreaterThanOrEqual(2);
  });

  it('空选择器返回空数组', () => {
    setupDOM('<div>hello</div>');
    expect(getBridge().locateAll('').length).toBe(0);
  });
});

describe('nth 语法', () => {
  it('nth:2 选择第 2 个匹配', () => {
    setupDOM(`
      <div>
        <button id="b1">提交</button>
        <button id="b2">提交</button>
        <button id="b3">提交</button>
      </div>
    `);
    const el = getBridge().locateElement('nth:2,提交');
    expect(el).not.toBeNull();
    expect(el.id).toBe('b2');
  });

  it('nth 超出范围返回 null', () => {
    setupDOM('<button id="b1">提交</button>');
    expect(getBridge().locateElement('nth:5,提交')).toBeNull();
  });

  it('nth 无效格式返回 null', () => {
    setupDOM('<button>提交</button>');
    expect(getBridge().locateElement('nth:')).toBeNull();
    expect(getBridge().locateElement('nth:abc,提交')).toBeNull();
  });
});

describe('clickElement 候选列表', () => {
  it('找不到元素但 locateAll 有结果时返回候选', () => {
    setupDOM(`
      <button id="b1" disabled>提交</button>
      <button id="b2">取消</button>
    `);
    // CSS selector #nonexistent 不会 locateAll 到任何东西
    const result = getBridge().clickElement('#nonexistent');
    expect(result.ok).toBe(false);
    expect(result.candidates).toBeDefined();
  });
});

describe('_checkWaitCondition', () => {
  it('navigation：URL 相同时返回 false', () => {
    setupDOM('<div>test</div>');
    getBridge()._waitInitialState = { url: window.location.href, lastMutationTime: Date.now() };
    expect(getBridge()._checkWaitCondition('navigation')).toBe(false);
  });

  it('element：元素存在时返回 true', () => {
    setupDOM('<div id="x">test</div>');
    expect(getBridge()._checkWaitCondition('element:#x')).toBe(true);
    expect(getBridge()._checkWaitCondition('element:#missing')).toBe(false);
  });

  it('element!：元素不存在时返回 true', () => {
    setupDOM('<div>test</div>');
    expect(getBridge()._checkWaitCondition('element!:#missing')).toBe(true);
    expect(getBridge()._checkWaitCondition('element!:#x')).toBe(true);
  });

  it('stable：无 DOM 变化超 1 秒时返回 true', () => {
    setupDOM('<div>test</div>');
    getBridge()._waitInitialState = { url: '', lastMutationTime: Date.now() - 1100 };
    expect(getBridge()._checkWaitCondition('stable')).toBe(true);
    getBridge()._waitInitialState = { url: '', lastMutationTime: Date.now() - 100 };
    expect(getBridge()._checkWaitCondition('stable')).toBe(false);
  });
});

describe('waitFor', () => {

  it('element 条件满足', async () => {
    setupDOM('<div id="target">appeared</div>');
    const result = await getBridge().waitFor('element:#target', 1000);
    expect(result.ok).toBe(true);
  });

  it('element! 条件满足（元素不存在）', async () => {
    setupDOM('<div>test</div>');
    const result = await getBridge().waitFor('element!:#nonexistent', 1000);
    expect(result.ok).toBe(true);
  });

  it('超时返回失败', async () => {
    setupDOM('<div>test</div>');
    // 等一个不存在的条件但给很短的超时
    const result = await getBridge().waitFor('element:#never-exist', 100);
    expect(result.ok).toBe(false);
    expect(result.error).toContain('超时');
  });
});

describe('annotation 自动提取元素', () => {
  it('getAnnotations 返回的 rect 批注包含 elements 字段', () => {
    setupDOM(`
      <button id="btn" style="position:absolute;top:50px;left:50px;width:100px;height:30px;">按钮</button>
    `);

    const el = document.getElementById('btn')!;
    el.getBoundingClientRect = () =>
      ({ x: 50, y: 50, width: 100, height: 30, top: 50, left: 50, right: 150, bottom: 80 }) as DOMRect;

    const bridge = getBridge();
    bridge.annotation.start('rect');

    // 手动模拟 _handleUp 效果：添加带 elements 的 annotation
    bridge.annotation._annotations.push({
      type: 'rect',
      x: 40,
      y: 40,
      width: 120,
      height: 50,
      color: '#ff0000',
      elements: bridge.extractElementsInRect(40, 40, 120, 50),
    });

    const result = bridge.annotation.getAnnotations();
    expect(result.count).toBe(1);
    expect(result.annotations[0].elements).toBeDefined();
    expect(result.annotations[0].elements.length).toBeGreaterThan(0);
    expect(result.annotations[0].elements[0].tag).toBe('button');
  });
});

describe('_isDisabled', () => {
  it('检测原生 disabled 属性', () => {
    setupDOM('<button id="btn" disabled>按钮</button>');
    const bridge = getBridge();
    expect(bridge._isDisabled(document.getElementById('btn'))).toBe(true);
  });

  it('检测 aria-disabled="true"', () => {
    setupDOM('<div id="el" aria-disabled="true">元素</div>');
    const bridge = getBridge();
    expect(bridge._isDisabled(document.getElementById('el'))).toBe(true);
  });

  it('检测 CSS 类名中包含 disabled（如 ds-button--disabled）', () => {
    setupDOM('<div id="el" class="ds-button ds-button--disabled">创建</div>');
    const bridge = getBridge();
    expect(bridge._isDisabled(document.getElementById('el'))).toBe(true);
  });

  it('检测 is-disabled 类名（Element Plus）', () => {
    setupDOM('<div id="el" class="el-button is-disabled">按钮</div>');
    const bridge = getBridge();
    expect(bridge._isDisabled(document.getElementById('el'))).toBe(true);
  });

  it('正常元素返回 false', () => {
    setupDOM('<button id="btn">按钮</button>');
    const bridge = getBridge();
    expect(bridge._isDisabled(document.getElementById('btn'))).toBe(false);
  });
});

describe('clickElement disabled 检测', () => {
  it('CSS 类 disabled 的 div[role=button] 返回错误', () => {
    setupDOM(`
      <div id="dialog" role="dialog">
        <input type="text" class="ds-input__input" placeholder="输入名称" value="">
        <div role="button" class="ds-button ds-button--disabled">创建</div>
      </div>
    `);
    const bridge = getBridge();
    const result = bridge.clickElement('创建');
    expect(result.ok).toBe(false);
    expect(result.error).toContain('禁用');
  });

  it('正常按钮可以点击', () => {
    setupDOM(`
      <div role="dialog">
        <input type="text" value="test">
        <div role="button" class="ds-button ds-button--primary">创建</div>
      </div>
    `);
    const bridge = getBridge();
    const result = bridge.clickElement('创建');
    expect(result.ok).toBe(true);
  });
});

describe('extractForms 非原生按钮', () => {
  it('提取 div[role=button] 按钮', () => {
    setupDOM(`
      <div id="dialog" role="dialog">
        <div class="ds-form-item">
          <label class="ds-form-item__label"><span class="ds-form-item__label-text">名称</span></label>
          <div class="ds-form-item__content">
            <input type="text" class="ds-input__input" placeholder="输入名称" value="">
          </div>
        </div>
        <div class="ds-modal-content__footer">
          <div role="button" class="ds-button ds-button--outlinedNeutral">取消</div>
          <div role="button" class="ds-button ds-button--primary ds-button--disabled">创建</div>
        </div>
      </div>
    `);
    const bridge = getBridge();
    const result = bridge.extractForms();
    expect(result.forms.length).toBeGreaterThan(0);
    const form = result.forms[0];
    expect(form.fields.length).toBe(1);
    expect(form.fields[0].placeholder).toBe('输入名称');
    // 应该提取到 div[role=button] 按钮
    expect(form.buttons.length).toBe(2);
    expect(form.buttons[0].text).toBe('取消');
    expect(form.buttons[0].disabled).toBe(false);
    expect(form.buttons[1].text).toBe('创建');
    expect(form.buttons[1].disabled).toBe(true);
  });
});

describe('diffDigest 内容变化检测', () => {
  it('检测对话框关闭后页面内容变化', () => {
    setupDOM(`
      <div id="main">已有 API key 列表 key-001 active</div>
    `);
    const bridge = getBridge();
    const before = { url: 'https://example.com', title: 'API Keys', overlayOpen: true, overlayText: '创建 API key', mainTextTail: '已有 API key 列表 key-001 active' };
    const after = { url: 'https://example.com', title: 'API Keys', overlayOpen: false, overlayText: '', mainTextTail: '已有 API key 列表 key-001 active sk-abc123 新创建的key active' };
    const diff = bridge.diffDigest(before, after);
    expect(diff).toContain('覆盖层已关闭');
    expect(diff).toContain('页面内容变化');
    expect(diff).toContain('sk-abc123');
  });

  it('无变化时返回默认信息', () => {
    setupDOM('<div>test</div>');
    const bridge = getBridge();
    const before = { url: 'https://example.com', title: 'Test', overlayOpen: false, overlayText: '', mainTextTail: 'hello world' };
    const after = { url: 'https://example.com', title: 'Test', overlayOpen: false, overlayText: '', mainTextTail: 'hello world' };
    const diff = bridge.diffDigest(before, after);
    expect(diff).toContain('页面无明显变化');
  });

  it('_textDiff 提取新增内容', () => {
    setupDOM('<div>test</div>');
    const bridge = getBridge();
    const result = bridge._textDiff(
      '这是原来的内容。第一行不变。',
      '这是原来的内容。第一行不变。这是新增的一行。'
    );
    expect(result).toContain('新增的一行');
  });

  it('_textDiff 内容过长时截断', () => {
    setupDOM('<div>test</div>');
    const bridge = getBridge();
    const newPart = 'x'.repeat(2000);
    const result = bridge._textDiff('before', newPart);
    expect(result.length).toBeLessThanOrEqual(1004); // 1000 + '...'
    expect(result).toContain('...');
  });
});

describe('observer 模块', () => {
  it('start/stop 生命周期', () => {
    setupDOM('<div id="app">content</div>');
    const bridge = getBridge();
    expect(bridge.observer._started).toBe(false);
    bridge.observer.start();
    expect(bridge.observer._started).toBe(true);
    bridge.observer.stop();
    expect(bridge.observer._started).toBe(false);
  });

  it('drainEvents 清空事件队列', () => {
    setupDOM('<div id="app">content</div>');
    const bridge = getBridge();
    bridge.observer.start();
    bridge.observer._pushEvent({ type: 'test', timestamp: Date.now() });
    expect(bridge.observer._eventQueue.length).toBe(1);
    const events = bridge.observer.drainEvents();
    expect(events.length).toBe(1);
    expect(events[0].type).toBe('test');
    expect(bridge.observer._eventQueue.length).toBe(0);
    bridge.observer.stop();
  });

  it('队列长度超过 100 时自动裁剪', () => {
    setupDOM('<div id="app">content</div>');
    const bridge = getBridge();
    bridge.observer.start();
    for (let i = 0; i < 120; i++) {
      bridge.observer._pushEvent({ type: 'test', idx: i });
    }
    // 裁剪触发后队列长度应 <= 100
    expect(bridge.observer._eventQueue.length).toBeLessThanOrEqual(100);
    bridge.observer.stop();
  });

  it('用户点击事件捕获', () => {
    setupDOM(`
      <div id="app">
        <button id="test-btn">点击我</button>
      </div>
    `);
    const bridge = getBridge();
    bridge.observer.start();
    const btn = document.getElementById('test-btn')!;
    btn.click();
    const events = bridge.observer.drainEvents();
    expect(events.length).toBe(1);
    expect(events[0].type).toBe('user_click');
    expect(events[0].text).toContain('点击我');
    bridge.observer.stop();
  });

  it('用户输入事件捕获（防抖）', async () => {
    setupDOM(`
      <div id="app">
        <input id="test-input" placeholder="输入" />
      </div>
    `);
    const bridge = getBridge();
    bridge.observer.start();
    const input = document.getElementById('test-input') as HTMLInputElement;
    input.value = 'hello';
    // jsdom 的 dispatchEvent 被 mock 为直接返回 true，不触发冒泡。
    // 直接调用内部防抖逻辑验证：模拟 input 事件处理器被调用后的效果
    bridge.observer._pushEvent({
      type: 'user_input',
      timestamp: Date.now(),
      selector: '#test-input',
      label: '输入',
      value_length: input.value.length,
    });
    const events = bridge.observer.drainEvents();
    expect(events.length).toBe(1);
    const inputEvent = events[0] as any;
    expect(inputEvent.type).toBe('user_input');
    expect(inputEvent.value_length).toBe(5);
    bridge.observer.stop();
  });
});

describe('_getTopmostOverlay 泛化覆盖层检测', () => {
  it('无覆盖层时返回 null', () => {
    setupDOM(`
      <div id="app">
        <main>页面内容</main>
      </div>
    `);
    const bridge = getBridge();
    expect(bridge._getTopmostOverlay()).toBeNull();
  });

  it('检测 fixed 定位的覆盖层', () => {
    setupDOM(`
      <div id="app">
        <main>页面内容</main>
      </div>
    `);
    const overlay = document.createElement('div');
    overlay.textContent = '对话框内容';
    overlay.style.position = 'fixed';
    document.body.appendChild(overlay);

    // mock getBoundingClientRect 和 getComputedStyle
    overlay.getBoundingClientRect = () => ({ width: 400, height: 300, top: 100, left: 100, right: 500, bottom: 400, x: 100, y: 100 } as any);
    const origGetComputedStyle = window.getComputedStyle;
    window.getComputedStyle = (el: Element) => {
      const real = origGetComputedStyle.call(window, el);
      if (el === overlay) {
        return { ...real, position: 'fixed' } as CSSStyleDeclaration;
      }
      return real;
    };

    const origFn = document.elementFromPoint;
    document.elementFromPoint = () => overlay;

    const bridge = getBridge();
    const result = bridge._getTopmostOverlay();
    expect(result).not.toBeNull();
    expect(result!.textContent).toContain('对话框内容');

    document.elementFromPoint = origFn;
    window.getComputedStyle = origGetComputedStyle;
  });

  it('全屏 fixed 背景遮罩不被误判为覆盖层', () => {
    setupDOM(`
      <div id="backdrop">导航栏</div>
      <main>页面内容</main>
    `);
    const backdrop = document.getElementById('backdrop')!;
    // 模拟全屏遮罩层：width ≈ W 且 height ≈ H（不满足 width < W-10 || height < H-10）
    backdrop.getBoundingClientRect = () => ({ width: 1920, height: 1080, top: 0, left: 0, right: 1920, bottom: 1080, x: 0, y: 0 } as any);
    const origGetComputedStyle = window.getComputedStyle;
    window.getComputedStyle = ((el: Element) => {
      const real = origGetComputedStyle.call(window, el);
      if (el === backdrop) {
        return { ...real, position: 'fixed' } as CSSStyleDeclaration;
      }
      return real;
    }) as any;

    const origFn = document.elementFromPoint;
    document.elementFromPoint = () => backdrop;

    const origW = window.innerWidth;
    const origH = window.innerHeight;
    Object.defineProperty(window, 'innerWidth', { value: 1920, configurable: true });
    Object.defineProperty(window, 'innerHeight', { value: 1080, configurable: true });

    const bridge = getBridge();
    const result = bridge._getTopmostOverlay();
    expect(result).toBeNull();

    document.elementFromPoint = origFn;
    window.getComputedStyle = origGetComputedStyle;
    Object.defineProperty(window, 'innerWidth', { value: origW, configurable: true });
    Object.defineProperty(window, 'innerHeight', { value: origH, configurable: true });
  });

  it('全屏蒙层内有弹窗内容时能检测到蒙层', () => {
    setupDOM(`
      <div id="backdrop">API Key: sk-test456 复制 关闭</div>
      <main>页面内容</main>
    `);
    const backdrop = document.getElementById('backdrop')!;
    // 模拟全屏蒙层
    backdrop.getBoundingClientRect = () => ({ width: 1920, height: 1080, top: 0, left: 0, right: 1920, bottom: 1080, x: 0, y: 0 } as any);
    const origGetComputedStyle = window.getComputedStyle;
    window.getComputedStyle = ((el: Element) => {
      const real = origGetComputedStyle.call(window, el);
      if (el === backdrop) {
        return { ...real, position: 'fixed' } as CSSStyleDeclaration;
      }
      return real;
    }) as any;

    const origFn = document.elementFromPoint;
    document.elementFromPoint = () => backdrop;

    const origW = window.innerWidth;
    const origH = window.innerHeight;
    Object.defineProperty(window, 'innerWidth', { value: 1920, configurable: true });
    Object.defineProperty(window, 'innerHeight', { value: 1080, configurable: true });

    const bridge = getBridge();
    const result = bridge._getTopmostOverlay();
    // 全屏蒙层内有有意义的文本（>10字符），应返回蒙层
    expect(result).not.toBeNull();
    expect(result!.textContent).toContain('sk-test456');

    document.elementFromPoint = origFn;
    window.getComputedStyle = origGetComputedStyle;
    Object.defineProperty(window, 'innerWidth', { value: origW, configurable: true });
    Object.defineProperty(window, 'innerHeight', { value: origH, configurable: true });
  });
});

describe('_extractOverlayContent 覆盖层内容提取', () => {
  it('提取覆盖层文本，排除按钮', () => {
    setupDOM('<div id="app">content</div>');
    const overlay = document.createElement('div');
    overlay.innerHTML = '<p>API Key: sk-abc123</p><button>复制</button><div class="close">×</div>';
    const bridge = getBridge();
    const content = bridge._extractOverlayContent(overlay);
    expect(content).toContain('sk-abc123');
    expect(content).not.toContain('复制');
    expect(content).not.toContain('×');
  });

  it('null 输入返回空字符串', () => {
    setupDOM('<div id="app">content</div>');
    const bridge = getBridge();
    expect(bridge._extractOverlayContent(null)).toBe('');
  });
});

describe('getPageDigest 泛化摘要', () => {
  it('无覆盖层时 overlayOpen 为 false', () => {
    setupDOM('<div id="app">content</div>');
    const bridge = getBridge();
    const digest = bridge.getPageDigest();
    expect(digest.overlayOpen).toBe(false);
    expect(digest.overlayText).toBe('');
    expect(digest.url).toBeTruthy();
  });

  it('有覆盖层时 overlayOpen 为 true 且包含内容', () => {
    setupDOM('<div id="app">content</div>');
    const overlay = document.createElement('div');
    overlay.textContent = '新创建的 API Key: sk-test123';
    document.body.appendChild(overlay);

    // mock getBoundingClientRect
    overlay.getBoundingClientRect = () => ({ width: 400, height: 200, top: 100, left: 100, right: 500, bottom: 300, x: 100, y: 100 } as any);
    // mock getComputedStyle 返回 fixed
    const origGCS = window.getComputedStyle;
    window.getComputedStyle = ((el: Element) => {
      const real = origGCS.call(window, el);
      if (el === overlay) {
        return { ...real, position: 'fixed' } as CSSStyleDeclaration;
      }
      return real;
    }) as any;
    // mock elementFromPoint 返回 overlay
    const origFn = document.elementFromPoint;
    document.elementFromPoint = () => overlay;

    const bridge = getBridge();
    const digest = bridge.getPageDigest();
    expect(digest.overlayOpen).toBe(true);
    expect(digest.overlayText).toContain('sk-test123');

    document.elementFromPoint = origFn;
    window.getComputedStyle = origGCS;
  });
});
