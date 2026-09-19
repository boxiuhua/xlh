// Offline chart regression checks; no browser packages or market requests needed.
const fs = require('node:fs');
const assert = require('node:assert/strict');
const source = fs.readFileSync('src/web/page.rs', 'utf8');
for (const script of source.matchAll(/<script[^>]*>([\s\S]*?)<\/script>/g)) new Function(script[1]);
const start = source.indexOf('function stockHistorySeries(');
const end = source.indexOf('function forecastHtml(', start);
assert(start >= 0 && end > start);
const { series, render } = new Function('esc', source.slice(start, end) + ';return {series:stockHistorySeries,render:renderStockHistory};')(String);
const rows = Array.from({ length: 300 }, (_, i) => ({
  date: new Date(Date.UTC(2025, 0, i + 1)).toISOString().slice(0, 10), close: i + 1, adj_close: 2 * (i + 1)
}));
const computed = series(rows, 'close');
assert.equal(computed[18].ma20, null);
assert.equal(computed[19].ma20, 10.5);
assert.equal(computed[59].ma60, 30.5);
assert.equal(computed[299].ma20, 290.5);
assert.equal(series(rows, 'adj_close')[299].ma20, 581);
assert.equal(series(rows.slice().reverse(), 'close')[0].value, 1);
assert.equal(series([{ date: '2025-01-01', close: NaN }], 'close').length, 0);
function element(dataset = {}) {
  return { dataset, events: {}, attrs: {}, textContent: '', addEventListener(k, fn) { this.events[k] = fn; },
    setAttribute(k, v) { this.attrs[k] = v; }, getBoundingClientRect() { return { left: 0, width: 900 }; } };
}
const nodes = { svg: element(), '[data-readout]': element(), '[data-crosshair]': element(), '[data-basis]': element() };
const buttons = [60, 120, 250, 0].map(n => element({ window: String(n) }));
const container = { innerHTML: '', querySelector(k) { return nodes[k]; }, querySelectorAll() { return buttons; } };
render(container, { history: rows, currency: 'USD', market: 'US' });
assert.match(container.innerHTML, /120 条日线/);
assert.match(nodes['[data-readout]'].textContent, /MA20 290.500/);
buttons[0].events.click();
assert.match(container.innerHTML, /60 条日线/);
assert.match(nodes['[data-readout]'].textContent, /MA60 270.500/);
buttons[3].events.click();
assert.match(container.innerHTML, /300 条日线/);
nodes['[data-basis]'].events.change.call({ value: 'adj_close' });
assert.match(nodes['[data-readout]'].textContent, /600.000 USD/);
nodes.svg.events.keydown({ key: 'ArrowLeft', preventDefault() {} });
assert.match(nodes['[data-readout]'].textContent, /598.000 USD/);
render(container, { history: [{ date: '2025-01-01', close: 10, adj_close: 10 }] });
assert(!/NaN|Infinity/.test(container.innerHTML));
assert.match(container.innerHTML, /<circle/);
render(container, { history: [] });
assert.match(container.innerHTML, /暂无历史价格数据/);
console.log('PASS: chart syntax, MA values, windows, price basis, keyboard, single-point and empty data.');
