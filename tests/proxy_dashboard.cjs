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
const session = (carry, traditional, available = true) => ({
  id: 'a'.repeat(64), primary: {}, shadow: {}, context: {},
  cost_estimate: { carry_cost: available ? carry : null, traditional_cost: available ? traditional : null,
    savings: available ? traditional - carry : null,
    savings_percent: available && traditional > 0 ? 100 * (traditional - carry) / traditional : null,
    traditional_compactions: 2, reviewer_cost: available ? 0.02 : null, coverage: {} },
});
const total = (carry, traditional, savings, percent, included, excluded = 0, failed = 0) => ({
  carry_cost: carry, traditional_cost: traditional, savings, savings_percent: percent,
  included_sessions: included, excluded_sessions: excluded, excluded_failed_calls: failed,
});
let data = { sessions: [session(1, 2), session(3, 4)], unavailable_sessions: 0,
  cost_estimate: total(4, 6, 2, 100 / 3, 2),
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
  assert.equal(cards().get('Estimated savings · all time'), '$2.000000 (33.3%)');
  assert.ok(elements.get('assumptions').textContent.includes('272,000'));
  // Deliberately distinct from the session sum: the backend owns aggregation,
  // amounts, percentage and coverage. The browser only formats its response.
  data.cost_estimate = total(7, 10, 3, 30, 1, 4);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Carry scenario'), '$7.000000', 'render backend total instead of resumming sessions');
  assert.equal(cards().get('Estimated savings · all time'), '$3.000000 (30.0%)');
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 1 of 5 sessions'));
  data.sessions.push(session(100, 200, false));
  data.cost_estimate = total(4, 6, 2, 100 / 3, 2, 1);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated savings · all time'), '$2.000000 (33.3%)');
  assert.equal(cards().get('Carry scenario'), '$4.000000');
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 2 of 3 sessions'));
  assert.equal(elements.get('sessions').children[2].children[3].textContent, 'Estimated savings: Unavailable');
  const partial = session(1, 2);
  partial.cost_estimate.excluded_failed_calls = 2;
  partial.cost_estimate.coverage.failed_calls = 2;
  partial.primary.unavailable_cost_calls = 2;
  data.sessions = [partial, session(3, 4), session(100, 200, false)];
  data.cost_estimate = total(4, 6, 2, 100 / 3, 2, 1, 2);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated savings · all time'), '$2.000000 (33.3%)');
  assert.equal(cards().get('Carry scenario'), '$4.000000');
  assert.equal(cards().get('API-equivalent total'), 'Incomplete', 'excluding failures must not relabel observed usage completeness');
  assert.match(elements.get('estimate-coverage').textContent, /successful requests only.*2 failed calls excluded/i);
  assert.match(elements.get('sessions').children[0].children[3].textContent, /successful requests only/i);
  partial.cost_estimate = {...partial.cost_estimate, savings: null, carry_cost: null, traditional_cost: null, savings_percent: null};
  data.cost_estimate = total(3, 4, 1, 25, 1, 2);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Carry scenario'), '$3.000000');
  assert.ok(!elements.get('sessions').children[0].children[3].textContent.includes('successful requests only'), 'unavailable estimates never display partial savings');
  const unpriced = session(1, 2);
  unpriced.cost_estimate.excluded_unpriced_calls = 3;
  unpriced.cost_estimate.excluded_unpriced_reviewer_calls = 1;
  unpriced.cost_estimate.coverage.unmodeled_calls = 2;
  unpriced.primary.unavailable_cost_calls = 2;
  data.sessions = [unpriced];
  // Different from the session counts: the browser must not recompute coverage.
  data.cost_estimate = {...total(1, 2, 1, 50, 1), excluded_unpriced_calls: 5,
    excluded_unpriced_reviewer_calls: 1};
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated savings · all time'), '$1.000000 (50.0%)', 'show the backend priced subset despite omitted calls');
  assert.equal(cards().get('Unpriced calls excluded'), '5 (1 reviewer-only)');
  assert.match(elements.get('estimate-coverage').textContent, /5 unpriced calls excluded.*1 reviewer-only/i);
  assert.match(elements.get('estimate-coverage').textContent, /approximate/i);
  assert.match(elements.get('sessions').children[0].children[3].textContent, /priced calls only.*3 unpriced calls excluded.*1 reviewer-only/i);
  assert.equal(cards().get('API-equivalent total'), 'Incomplete', 'estimated comparison coverage must not hide unavailable observed usage');
  data.sessions = [session(3, 2)];
  data.cost_estimate = total(3, 2, -1, -50, 1);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated savings · all time'), '$-1.000000 (-50.0%)', 'do not clamp estimated losses to zero');
  data.unavailable_sessions = 1;
  data.cost_estimate = total(3, 2, -1, -50, 1, 1);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Traditional scenario'), '$2.000000', 'unreadable checkpoints do not suppress eligible estimates');
  assert.equal(cards().get('API-equivalent total'), 'Incomplete', 'usage totals must still flag unreadable checkpoints');
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 1 of 2 sessions'));
  data.unavailable_sessions = 0;
  data.sessions = [session(100, 200, false), { ...session(1, 2), cost_estimate: undefined }];
  data.cost_estimate = total(null, null, null, null, 0, 2);
  await vm.runInContext('refresh()', sandbox);
  for (const label of ['Carry scenario', 'Traditional scenario', 'Estimated savings · all time']) {
    assert.equal(cards().get(label), 'Unavailable', 'null amounts never become zero');
  }
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 0 of 2 sessions'));
  const allUnpriced = session(0, 0, false);
  allUnpriced.cost_estimate.excluded_unpriced_calls = 2;
  allUnpriced.cost_estimate.coverage.unmodeled_calls = 2;
  data.sessions = [allUnpriced];
  data.cost_estimate = {...total(null, null, null, null, 0, 1), excluded_unpriced_calls: 2};
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated savings · all time'), 'Unavailable', 'no priced primary observations must not become zero savings');
  assert.equal(cards().get('Unpriced calls excluded'), '2', 'track exclusions even if the entire estimate is unavailable');
  const noteTexts = el => [el.textContent, ...(el.children || []).flatMap(noteTexts)];
  assert.match(noteTexts(elements.get('sessions')).join('\n'), /2 unpriced calls excluded/i, 'unavailable session diagnostics retain omission counts');
  data.sessions = [session(0, 0)];
  data.cost_estimate = total(0, 0, 0, null, 1);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated savings · all time'), '$0.000000', 'valid zero is not unavailable or NaN');
  assert.equal(elements.get('sessions').children[0].children[3].textContent, 'Estimated savings: $0.000000');
  data.sessions = [
    {...session(1,2), id:'root', title:'Shared task', completed_requests:4, client:{kind:'conversation',agent_name:'/root'},
      planner:{candidate:{keep:1,compact:0.8,savings:0.2},min_payback_percent:10,reviewer_estimated_tokens:120000, reviewer_required_tokens:153160, reviewer_context_tokens:300000}},
    {...session(1,2), id:'child', title:'Shared task', completed_requests:2, client:{kind:'agent',agent_name:'/root/diagnosis'}},
    {...session(1,2), id:'guardian', title:'Internal transcript', completed_requests:3, client:{kind:'background'}}
  ];
  data.cost_estimate = total(3, 6, 3, 50, 3);
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
  data.cost_estimate = total(null, null, null, null, 0);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated savings · all time'), 'Unavailable');
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 0 of 0 sessions'));
  data.sessions = [{...session(1, 2), title:'Action identity', planner:{
    selected:true, selected_candidate_id:'stable-reviewer', selected_action:'reviewer_only',
    mode:'compact', payoff_requests:3, min_payback_percent:0,
    primary_output_allowance_tokens:500,
    primary_output_allowance_basis:'request_max_output_tokens_cap_sensitivity_not_expected_generation',
    candidate:{candidate_id:'stable-reviewer', action:'reviewer_only', keep:2, compact:1.5, savings:0.5}
  }}];
  await vm.runInContext('refresh()', sandbox);
  assert.ok(walk(elements.get('sessions')).some(n=>/reviewer.only cleanup selected/i.test(n.textContent)), 'render the actual selected action, not generic primary compaction');
  assert.ok(walk(elements.get('sessions')).some(n=>/500.*cap sensitivity.*not expected generation/i.test(n.textContent)), 'explicit output cap is sensitivity, not a usage prediction');
  data.sessions[0].planner.primary_output_allowance_tokens=0;
  data.sessions[0].planner.primary_output_allowance_basis='legacy_zero_no_explicit_request_cap_not_headroom';
  await vm.runInContext('refresh()', sandbox);
  assert.ok(walk(elements.get('sessions')).some(n=>/zero.*no explicit.*cap/i.test(n.textContent)), 'missing request cap must not silently mean expected headroom output');
  data.sessions = [session(1, 2)];
  data.sessions[0].cost_estimate.accounting_basis = 'legacy_json_v1';
  data.cost_estimate = {...total(null, null, null, null, 0, 1), legacy_sessions: 1};
  await vm.runInContext('refresh()', sandbox);
  assert.match(walk(elements.get('sessions')).map(n=>n.textContent).join('\n'), /legacy.*JSON/i, 'old monetary sums need an explicit legacy label');
  assert.match(elements.get('estimate-coverage').textContent, /1 legacy/i, 'do not blend legacy and receipt-anchored windows');
  data.sessions[0].cost_estimate.accounting_basis = 'receipt_anchored_v2';
  Object.assign(data.sessions[0].cost_estimate, {matched_primary_responses:7,
    observed_primary_cost:0.125, observed_reviewer_cost:null,
    unavailable_primary_cost_calls:2, unavailable_reviewer_cost_calls:1});
  data.cost_estimate = {...total(1, 2, 1, 50, 1), matched_primary_responses:42,
    unavailable_primary_cost_calls:3, unavailable_reviewer_cost_calls:4};
  await vm.runInContext('refresh()', sandbox);
  assert.match(walk(elements.get('sessions')).map(n=>n.textContent).join('\n'), /7 matched primary.*observed.*subtotal.*\$0.125000.*2 primary.*1 reviewer/i);
  assert.match(elements.get('estimate-coverage').textContent, /42 matched primary.*3 primary.*4 reviewer/i, 'backend owns aggregate observed-cost completeness and matched coverage too');
  assert.match(elements.get('assumptions').textContent, /provider.*input/i);
  assert.match(elements.get('assumptions').textContent, /estimated.*restor/i);
  assert.ok(!/non-tool|reviewer inputs priced uncached/.test(elements.get('assumptions').textContent));
  console.log('proxy dashboard scenarios: ok');
})().catch(error => { console.error(error); process.exitCode = 1; });
