const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const element = () => ({
  children: [],
  classList: { add() {} },
  append(...items) { this.children.push(...items); },
  addEventListener() {},
  querySelector() { return null; },
});
const elements = new Map();
const document = {
  querySelector(key) {
    if (!elements.has(key)) elements.set(key, element());
    return elements.get(key);
  },
  createElement: element,
  createTextNode(value) { return { textContent: value }; },
  documentElement: { scrollHeight: 0 },
};
const sandbox = {
  document,
  window: { addEventListener() {}, innerHeight: 0, scrollTo() {}, scrollY: 0, location: 'http://localhost/' },
  setInterval() {},
  setTimeout() { return 0; },
  clearTimeout() {},
  requestAnimationFrame() {},
  MutationObserver: class { observe() {} },
  EventSource: class {},
  URL,
};
vm.createContext(sandbox);
const html = fs.readFileSync(path.join(process.cwd(), 'src/web/index.html'), 'utf8');
vm.runInContext(html.match(/<script>([\s\S]*?)<\/script>/)[1], sandbox);
vm.runInContext(`
const replayed = {run_id:'test', seq:1, event:'model_response', data:{usage:{total_tokens:10}}};
event(replayed); event({event:'history_complete'});
event(replayed); event({event:'history_complete'});
event({run_id:'recovery', seq:1, event:'model_response', data:{usage:{total_tokens:20}}});
event({event:'model_progress', data:{output_tokens:1, output_events:1}});
`, sandbox);
assert.equal(
  vm.runInContext('totals.steps', sandbox),
  2,
  'replaying a durable SSE event after reconnect must not double-count model calls',
);
assert.equal(
  vm.runInContext('totals.total', sandbox),
  30,
  'a recovery segment with its own run ID must remain visible',
);
