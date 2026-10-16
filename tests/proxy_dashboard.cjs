const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const element = (tagName = '') => ({
  tagName,
  children: [], textContent: '', style: {}, setAttribute() {},
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
const cards = () => new Map([...elements.get('cards').children,...elements.get('cost-cards').children].map(card => card.children.map(item => item.textContent)));
(async () => {
  await new Promise(resolve => setImmediate(resolve));
  data.sessions[0].title='Older task'; data.sessions[0].updated_at=1;
  data.sessions[1].title='Newest task'; data.sessions[1].updated_at=2;
  await vm.runInContext('refresh()', sandbox);
  assert.equal(elements.get('sessions').children[0].children[0].textContent, 'Newest task', 'show titles and most recent sessions first');
  assert.equal(elements.get('cards').children.length, 3, 'overview focuses on savings, sessions and completed requests');
  assert.equal(cards().get('Carry scenario'), '$4.000000');
  assert.equal(cards().get('Traditional scenario'), '$6.000000');
  assert.equal(cards().get('Estimated savings · all time'), '$2.000000 (33.3%)', 'percentage uses summed traditional denominator');
  assert.ok(elements.get('assumptions').textContent.includes('272,000'));
  data.sessions.push(session(100, 200, false));
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated savings · all time'), '$2.000000 (33.3%)', 'exclude incomplete trajectories from both branches');
  assert.equal(cards().get('Carry scenario'), '$4.000000');
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 2 of 3 sessions'));
  assert.equal(elements.get('sessions').children[2].children[3].textContent, 'Estimated savings: Incomplete');
  assert.equal(elements.get('sessions').children[2].children[3].textContent, 'Estimated savings: Incomplete');
  data.sessions = [session(3, 2)];
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated savings · all time'), '$-1.000000 (-50.0%)', 'do not clamp estimated losses to zero');
  data.unavailable_sessions = 1;
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Traditional scenario'), '$2.000000', 'unreadable checkpoints do not suppress eligible estimates');
  assert.equal(cards().get('API-equivalent total'), 'Incomplete', 'usage totals must still flag unreadable checkpoints');
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 1 of 2 sessions'));
  data.unavailable_sessions = 0;
  data.sessions = [session(100, 200, false), { ...session(1, 2), cost_estimate: undefined }];
  await vm.runInContext('refresh()', sandbox);
  for (const label of ['Carry scenario', 'Traditional scenario', 'Estimated savings · all time']) {
    assert.equal(cards().get(label), label==='Estimated savings · all time'?'Not enough data':'No eligible sessions');
  }
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 0 of 2 sessions'));
  data.sessions = [
    {...session(1,2), id:'root', title:'Shared task', completed_requests:4, client:{kind:'conversation',agent_name:'/root'},
      planner:{candidate:{keep:1,compact:0.8,savings:0.2},min_payback_percent:10,reviewer_estimated_tokens:120000, reviewer_required_tokens:153160, reviewer_context_tokens:300000}},
    {...session(1,2), id:'child', title:'Shared task', completed_requests:2, client:{kind:'agent',agent_name:'/root/diagnosis'}},
    {...session(1,2), id:'guardian', title:'Internal transcript', completed_requests:3, client:{kind:'background'}}
  ];
  await vm.runInContext('refresh()', sandbox);
  assert.equal(elements.get('sessions').children.length, 1, 'same-title conversation histories share a visual group, not accounting');
  assert.equal(elements.get('background-sessions').children.length, 1, 'client background traffic is kept separate');
  assert.equal(cards().get('Completed requests'), '9', 'all histories still count toward totals');
  assert.equal(cards().get('Carry scenario'), '$3.000000', 'grouping must not drop or duplicate receipts');
  const walk = el => [el, ...(el.children || []).flatMap(walk)];
  const nodes = walk(elements.get('sessions'));
  assert.equal(nodes.filter(n=>n.tagName==='progress').length, 1, 'keep context budget bars, but no duplicate-looking payoff progress bar');
  assert.ok(nodes.some(n=>n.textContent.includes('Agent · diagnosis')), 'agent role is client-reported, not guessed from titles');
  assert.ok(nodes.some(n=>n.textContent==='120k'), 'display estimated input separately from capacity');
  assert.ok(nodes.some(n=>n.textContent.includes('153.2k reserved of 300k budget')), '300k is a budget, not token usage');
  assert.ok(nodes.some(n=>n.textContent.includes('diagnosis')), 'distinguish inherited subagent histories');
  data.sessions[1].client=null;
  await vm.runInContext('refresh()', sandbox);
  assert.ok(walk(elements.get('sessions')).some(n=>n.textContent.includes('Unclassified · child')), 'a shared title alone must never imply an agent role');
  data.sessions[1].title='Unrelated task';
  await vm.runInContext('refresh()', sandbox);
  assert.equal(elements.get('sessions').children.length, 2, 'different titles remain separate');
  data.sessions = [];
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated savings · all time'), 'Not enough data');
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 0 of 0 sessions'));
  console.log('proxy dashboard scenarios: ok');
})().catch(error => { console.error(error); process.exitCode = 1; });
