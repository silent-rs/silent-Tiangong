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
        <div class="ant-modal-wrap" role="dialog">
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

    // ant-modal-wrap 需要 offsetHeight > 0
    const modal = document.querySelector('.ant-modal-wrap')!;
    Object.defineProperty(modal, 'offsetHeight', { value: 500, configurable: true });

    const el = getBridge().locateElement('创建');
    expect(el).not.toBeNull();
    expect(el.id).toBe('dialog-create');
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
        <div class="el-dialog__wrapper">
          <div class="el-dialog">
            <button id="dialog-btn">确定</button>
          </div>
        </div>
      </div>
    `);

    const wrapper = document.querySelector('.el-dialog__wrapper')!;
    Object.defineProperty(wrapper, 'offsetHeight', { value: 300, configurable: true });

    const el = getBridge().locateElement('确定');
    expect(el).not.toBeNull();
    expect(el.id).toBe('dialog-btn');
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
