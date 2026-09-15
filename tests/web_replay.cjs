const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');

const element = () => ({
  children: [],
  classList: { add() {} },
  replaceChildren(...items) { this.children = items; this.textContent = ""; },
  append(...items) { this.children.push(...items); },
  remove() { this.removed = true; },
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
vm.runInContext(`event({event:'model_progress', data:{preview:'# Hello', output_tokens:2, output_events:2}});`, sandbox);
assert.equal(vm.runInContext('modelProgress.children[0]?.className', sandbox), 'markdown', 'live text should render Markdown');
vm.runInContext(`event({event:'model_response', data:{usage:{}}});`, sandbox);
assert.equal(vm.runInContext('modelProgress', sandbox), null);
assert.ok(![...elements.values()].flatMap(el => el.children).some(el => el.textContent === 'Model response received'), 'completion should not leave a redundant response-received item');

(async () => {
  sandbox.fetch = () => new Promise(() => {});
  vm.runInContext("text.value='please add tests'; send();", sandbox);
  assert.equal(vm.runInContext('pendingMessages.length', sandbox), 1);
  assert.ok(elements.get('#queued')?.children.includes(vm.runInContext('pendingMessages[0].el', sandbox)), 'pending input must stay beside composer, outside timeline');
  assert.equal(vm.runInContext('pendingMessages[0].el.textContent', sandbox), 'You (sending): please add tests');
  const draft=vm.runInContext('pendingMessages[0].el', sandbox);
  vm.runInContext("event({event:'human_message',data:{message:'please add tests'}})", sandbox);
  assert.equal(draft.removed,true);
  assert.ok(elements.get('#activity').children.some(el=>el.textContent==='You: please add tests'));
  assert.equal(vm.runInContext('pendingMessages.length', sandbox), 0);
})().catch(error => { console.error(error); process.exitCode = 1; });
vm.runInContext(`
const usageEvent={run_id:'usage',seq:1,event:'model_response',data:{step:4,usage:{input_tokens:1200,cached_input_tokens:800,output_tokens:75,total_tokens:1275}}};
event(usageEvent);event(usageEvent);
`, sandbox);
const usageLines = [...elements.values()].flatMap(el => el.children).filter(el => el.className === 'entry response-usage muted');
assert.equal(usageLines.filter(el => el.textContent === 'Response 4 · 1200 input (800 cached) · 75 output · 1275 total tokens').length, 1, 'per-response usage must be visible and deduplicated on replay');
assert.ok(!html.includes('id="stat-time"') && !html.includes('id="stat-tokens"'), 'footer should not contain timer or cumulative tokens');
vm.runInContext(`
totals.cost=0;totals.costUnknown=false;
const costEvent={run_id:'cost',seq:1,event:'model_response',data:{usage:{},estimated_cost_usd:0.258}};
event(costEvent);event(costEvent);
`, sandbox);
assert.equal(elements.get('#stat-cost').textContent, '$0.2580');
vm.runInContext(`event({event:'model_response',data:{usage:{},estimated_cost_usd:null}});`, sandbox);
vm.runInContext('totals.total=2520000;totals.cached=2400000;totals.output=120000;renderStats()', sandbox);
assert.equal(elements.get('#stat-cost').textContent, '2.5m tokens (2.4m cached, 120k out)');
