const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const element = () => ({
  children: [], textContent: '',
  replaceChildren(...items) { this.children = items; },
  append(...items) { this.children.push(...items); },
  addEventListener() {},
});
const elements = new Map();
const document = {
  getElementById(id) {
    if (!elements.has(id)) elements.set(id, element());
    return elements.get(id);
  },
  createElement: element,
};
const session = (carry, traditional, priced = true) => ({
  id: 'a'.repeat(64), primary: {}, shadow: {}, context: {},
  cost_estimate: { priced, carry_cost: carry, traditional_cost: traditional,
    savings: traditional - carry, savings_percent: 100 * (traditional - carry) / traditional,
    traditional_compactions: 2, reviewer_cost: 0.02, coverage: {} },
});
let data = { sessions: [session(1, 2), session(3, 4)], unavailable_sessions: 0,
  cost_scenario: {context_window: 272000, reserve: 16384, keep_recent: 20000, summary: 1000} };
const sandbox = {
  document, location: {hash: ''}, history: {}, URLSearchParams,
  fetch: async () => ({ok: true, json: async () => data}), setInterval() {},
};
vm.createContext(sandbox);
const html = fs.readFileSync('src/proxy_dashboard.html', 'utf8');
vm.runInContext(html.match(/<script>([\s\S]*?)<\/script>/)[1], sandbox);
const cards = () => new Map(elements.get('cards').children.map(card => card.children.map(item => item.textContent)));
(async () => {
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(cards().get('Carry scenario'), '$4.000000');
  assert.equal(cards().get('Traditional scenario'), '$6.000000');
  assert.equal(cards().get('Estimated scenario saving'), '$2.000000 (33.3%)', 'percentage uses summed traditional denominator');
  assert.ok(elements.get('assumptions').textContent.includes('272,000'));
  data.sessions.push(session(100, 200, false));
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated scenario saving'), 'Unavailable', 'partial trajectories cannot masquerade as complete savings');
  assert.equal(cards().get('Carry scenario'), 'Unavailable');
  data.sessions = [session(3, 2)];
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated scenario saving'), '$-1.000000 (-50.0%)', 'do not clamp estimated losses to zero');
  data.unavailable_sessions = 1;
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Traditional scenario'), 'Unavailable');
  console.log('proxy dashboard scenarios: ok');
})().catch(error => { console.error(error); process.exitCode = 1; });
