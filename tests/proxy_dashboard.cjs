const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const element = (tagName = '') => ({
  tagName, parentElement: null,
  children: [], textContent: '', style: {}, attributes: new Map(),
  setAttribute(name, value) { this.attributes.set(name, String(value)); },
  getAttribute(name) { return this.attributes.get(name) ?? null; },
  getAttributeNames() { return [...this.attributes.keys()]; },
  removeAttribute(name) { this.attributes.delete(name); },
  replaceChildren(...items) { for (const child of this.children) child.parentElement=null; this.children=[]; this.append(...items); },
  append(...items) { for (const item of items) this.insertBefore(item, null); },
  insertBefore(item, next) {
    if (item === next) return;
    if (item.parentElement) {
      if (item.contains(document.activeElement)) document.activeElement = null;
      item.parentElement.removeChild(item);
    }
    const index=next ? this.children.indexOf(next) : this.children.length;
    this.children.splice(index,0,item); item.parentElement=this;
  },
  removeChild(item) { this.children.splice(this.children.indexOf(item),1); item.parentElement=null; },
  addEventListener() {},
  get firstElementChild() { return this.children[0] || null; },
  contains(node) { return this === node || this.children.some(child => child.contains(node)); },
  focus() { document.activeElement = this; },
});
const elements = new Map();
const document = {
  activeElement: null,
  getElementById(id) {
    const find = node => node.id === id ? node : node.children.map(find).find(Boolean);
    for (const root of elements.values()) {
      const found = find(root);
      if (found) return found;
    }
    if (!elements.has(id)) elements.set(id, element());
    return elements.get(id);
  },
  createElement: element,
};
let nextSession = 0;
const session = (carry, traditional, available = true) => ({
  id: (++nextSession).toString(16).padStart(64,'0'), primary: {}, shadow: {}, context: {},
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
  const walk = el => [el, ...(el.children || []).flatMap(walk)];
  const byClass = (el, name) => walk(el).find(n => n.className === name);
  const firstSession = () => byClass(elements.get('sessions'), 'session');
  const detail = byClass(firstSession(), 'session-details');
  assert.ok(detail, 'technical accounting and planner information has a Details disclosure');
  assert.ok(!detail.open, 'technical details start collapsed');
  assert.ok(byClass(detail, 'plan-summary'), 'planner mechanics stay inside the collapsed technical disclosure');
  assert.match(byClass(firstSession(), 'session-savings').textContent, /Estimated difference/);
  assert.match(elements.get('estimate-coverage').textContent, /Includes 2 of 2/);
  assert.ok(!byClass(firstSession(), 'session-savings').textContent.includes('Receipt'), 'primary outcome is not a technical accounting paragraph');
  data.sessions[0].title='Older task'; data.sessions[0].updated_at=1;
  data.sessions[1].title='Newest task'; data.sessions[1].updated_at=2;
  await vm.runInContext('refresh()', sandbox);
  assert.equal(walk(firstSession()).find(n=>n.tagName==='h3').textContent, 'Older task', 'stable history-ID order does not follow polling activity');
  assert.equal(elements.get('cards').children.length, 3, 'overview compares Carry, traditional and difference');
  assert.equal(cards().get('With Carry'), '$4.000000');
  assert.equal(cards().get('Traditional'), '$6.000000');
  assert.equal(cards().get('Estimated difference'), '$2.000000 (33.3%)');
  assert.match(elements.get('assumptions').textContent, /^Usage-based cost estimate at API rates/, 'explanation leads with plain meaning, not accounting version jargon');
  assert.ok(elements.get('assumptions').textContent.includes('272,000'));
  // Deliberately distinct from the session sum: the backend owns aggregation,
  // amounts, percentage and coverage. The browser only formats its response.
  data.cost_estimate = total(7, 10, 3, 30, 1, 4);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('With Carry'), '$7.000000', 'render backend total instead of resumming sessions');
  assert.equal(cards().get('Estimated difference'), '$3.000000 (30.0%)');
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 1 of 5 sessions'));
  data.sessions.push(session(100, 200, false));
  data.cost_estimate = total(4, 6, 2, 100 / 3, 2, 1);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated difference'), '$2.000000 (33.3%)');
  assert.equal(cards().get('With Carry'), '$4.000000');
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 2 of 3 sessions'));
  assert.equal(byClass(elements.get('sessions').children[2], 'session-savings').textContent, 'Estimated difference: Unavailable');
  const partial = session(1, 2);
  partial.cost_estimate.excluded_failed_calls = 2;
  partial.cost_estimate.coverage.failed_calls = 2;
  partial.primary.unavailable_cost_calls = 2;
  data.sessions = [partial, session(3, 4), session(100, 200, false)];
  data.cost_estimate = total(4, 6, 2, 100 / 3, 2, 1, 2);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated difference'), '$2.000000 (33.3%)');
  assert.equal(cards().get('With Carry'), '$4.000000');
  assert.equal(cards().get('API-equivalent total'), 'Incomplete', 'excluding failures must not relabel observed usage completeness');
  assert.match(elements.get('estimate-coverage').textContent, /successful requests only.*2 failed calls excluded/i);
  assert.match(byClass(firstSession(), 'diagnostics').textContent, /successful requests only/i);
  partial.cost_estimate = {...partial.cost_estimate, savings: null, carry_cost: null, traditional_cost: null, savings_percent: null};
  data.cost_estimate = total(3, 4, 1, 25, 1, 2);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('With Carry'), '$3.000000');
  assert.equal(byClass(firstSession(), 'session-savings').textContent, 'Estimated difference: Unavailable', 'unavailable estimates never display partial savings');
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
  assert.equal(cards().get('Estimated difference'), '$1.000000 (50.0%)', 'show the backend priced subset despite omitted calls');
  assert.equal(cards().get('Calls excluded from comparison'), '5 (1 reviewer-only)');
  assert.match(elements.get('estimate-coverage').textContent, /5 calls excluded from comparison.*1 reviewer-only/i);
  assert.match(elements.get('estimate-coverage').textContent, /approximate/i);
  assert.match(byClass(firstSession(), 'diagnostics').textContent, /priced calls only.*3 calls excluded from comparison.*1 reviewer-only/i);
  assert.equal(cards().get('API-equivalent total'), 'Incomplete', 'estimated comparison coverage must not hide unavailable observed usage');
  // Native ledgers and comparison accounting deliberately describe different
  // populations. Keep the known observed subtotal visible outside disclosures.
  Object.assign(unpriced.primary, {cost_usd: 7, unavailable_cost_calls: 3});
  Object.assign(unpriced.shadow, {cost_usd: 0.25, unavailable_cost_calls: 2});
  await vm.runInContext('refresh()', sandbox);
  assert.equal(elements.get('observed-cost-summary')?.textContent || '',
    'Observed API-equivalent cost: known subtotal $7.250000 · 5 unpriced calls (3 primary, 2 reviewer). Total unavailable.');
  assert.equal(cards().get('Observed priced subtotal'), '$7.250000');
  assert.equal(cards().get('With Carry'), '$1.000000', 'native subtotal does not replace comparison-window money');
  unpriced.primary.cost_usd = null; unpriced.shadow.cost_usd = null;
  await vm.runInContext('refresh()', sandbox);
  assert.match(elements.get('observed-cost-summary').textContent, /known subtotal Unavailable.*5 unpriced calls.*Total unavailable/);
  assert.equal(cards().get('Observed priced subtotal'), 'Unavailable', 'wholly unknown observed expense never becomes zero');
  assert.equal(elements.get('usage').children[0].children[7].textContent,
    'Known subtotal Unavailable · total unavailable', 'lane detail cannot invent a zero subtotal for null expense');
  unpriced.primary.cost_usd = 0; unpriced.shadow.cost_usd = 0;
  unpriced.primary.unavailable_cost_calls = 0; unpriced.shadow.unavailable_cost_calls = 0;
  await vm.runInContext('refresh()', sandbox);
  assert.equal(elements.get('observed-cost-summary').textContent, 'Observed API-equivalent total: $0.000000.');
  data.sessions = [session(3, 2)];
  data.cost_estimate = total(3, 2, -1, -50, 1);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated difference'), '$-1.000000 (-50.0%)', 'do not clamp estimated losses to zero');
  data.unavailable_sessions = 1;
  data.cost_estimate = total(3, 2, -1, -50, 1, 1);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Traditional'), '$2.000000', 'unreadable checkpoints do not suppress eligible estimates');
  assert.equal(cards().get('API-equivalent total'), 'Incomplete', 'usage totals must still flag unreadable checkpoints');
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 1 of 2 sessions'));
  data.unavailable_sessions = 0;
  data.sessions = [session(100, 200, false), { ...session(1, 2), cost_estimate: undefined }];
  data.cost_estimate = total(null, null, null, null, 0, 2);
  await vm.runInContext('refresh()', sandbox);
  for (const label of ['With Carry', 'Traditional', 'Estimated difference']) {
    assert.equal(cards().get(label), 'Unavailable', 'null amounts never become zero');
  }
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 0 of 2 sessions'));
  const allUnpriced = session(0, 0, false);
  allUnpriced.cost_estimate.excluded_unpriced_calls = 2;
  allUnpriced.cost_estimate.coverage.unmodeled_calls = 2;
  data.sessions = [allUnpriced];
  data.cost_estimate = {...total(null, null, null, null, 0, 1), excluded_unpriced_calls: 2};
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated difference'), 'Unavailable', 'no priced primary observations must not become zero savings');
  assert.equal(cards().get('Calls excluded from comparison'), '2', 'track exclusions even if the entire estimate is unavailable');
  const noteTexts = el => [el.textContent, ...(el.children || []).flatMap(noteTexts)];
  assert.match(noteTexts(elements.get('sessions')).join('\n'), /2 calls excluded from comparison/i, 'unavailable session diagnostics retain omission counts');
  data.sessions = [session(0, 0)];
  data.cost_estimate = total(0, 0, 0, null, 1);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated difference'), '$0.000000', 'valid zero is not unavailable or NaN');
  assert.equal(byClass(firstSession(), 'session-savings').textContent, 'Estimated difference: $0.000000');
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
  assert.match(elements.get('activity-summary').textContent, /9 completed requests/, 'all histories still count toward totals');
  assert.equal(cards().get('With Carry'), '$3.000000', 'grouping must not drop or duplicate receipts');
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
  data.sessions = [{...session(1,2), planner:{reviewer_estimated_tokens:350000,
    reviewer_required_tokens:352048, reviewer_context_tokens:1050000,
    reviewer_ordinary_context_tokens:272000}}];
  await vm.runInContext('refresh()', sandbox);
  assert.ok(walk(elements.get('sessions')).some(n=>n.tagName==='progress' && n.max===1050000), 'physical capacity is the planner denominator, not a role fallback');
  assert.ok(walk(elements.get('sessions')).some(n=>/272k ordinary economic target/.test(n.textContent)), 'show the separate catalog economic target without turning it into a hard cap');
  data.sessions = [];
  data.cost_estimate = total(null, null, null, null, 0);
  await vm.runInContext('refresh()', sandbox);
  assert.equal(cards().get('Estimated difference'), 'Unavailable');
  assert.ok(elements.get('estimate-coverage').textContent.includes('Includes 0 of 0 sessions'));
  assert.match(byClass(elements.get('sessions'), 'empty')?.textContent || '', /No conversations/, 'an empty dashboard explains why no cards appear');
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
  assert.match(elements.get('accounting-detail').textContent, /42 matched primary.*3 primary.*4 reviewer/i, 'backend owns aggregate observed-cost completeness and matched coverage too');
  assert.match(elements.get('estimate-coverage').textContent, /3 primary.*4 reviewer/i, 'comparison-accounting gaps stay visible without claiming native usage is unpriced');
  assert.doesNotMatch(elements.get('estimate-coverage').textContent, /unknown actual costs/i);
  assert.match(elements.get('accounting-detail').textContent, /comparison.*subtotal/i, 'receipt-window subtotal is not the all-usage subtotal');
  assert.match(elements.get('assumptions').textContent, /provider.*input/i);
  assert.match(elements.get('assumptions').textContent, /estimated.*restor/i);
  assert.ok(!/non-tool|reviewer inputs priced uncached/.test(elements.get('assumptions').textContent));
  // Comparison support does not establish observed-cost completeness. No
  // modality reason exists in this contract: never infer images from counts.
  const unsupported = session(0, 0, false);
  unsupported.primary = {cost_usd: 9, unavailable_cost_calls: 0};
  unsupported.shadow = {cost_usd: 0.5, unavailable_cost_calls: 0};
  unsupported.cost_estimate.excluded_unpriced_calls = 2;
  unsupported.cost_estimate.coverage = {unmodeled_calls: 2, failed_calls: 1,
    unknown_cache_requests: 3, missing_usage: 4, prior_untracked_calls: 5};
  data.sessions = [unsupported];
  data.cost_estimate = total(null, null, null, null, 0, 1);
  await vm.runInContext('refresh()', sandbox);
  const unsupportedNotes = byClass(firstSession(), 'diagnostics').textContent;
  assert.match(unsupportedNotes, /2 calls excluded from comparison/i);
  assert.doesNotMatch(unsupportedNotes, /2 unpriced calls/i, 'unmodeled comparisons are not proof of unknown observed expense');
  assert.match(unsupportedNotes, /comparison.*2 unmodeled calls.*reason not reported/i, 'unsupported comparisons must not be hidden by a failure');
  for (const reason of [/failed call/i, /unknown model or cache/i, /missing usage/i, /earlier calls.*not tracked/i]) {
    assert.match(unsupportedNotes, reason, 'independent coverage reasons coexist');
  }
  assert.doesNotMatch(unsupportedNotes, /image/i, 'missing modality provenance cannot be invented in UI');
  assert.equal(cards().get('API-equivalent total'), '$9.500000', 'unsupported comparison can still have complete observed cost');
  assert.equal(cards().get('Estimated difference'), 'Unavailable');
  assert.equal(elements.get('observed-cost-summary').textContent, 'Observed API-equivalent total: $9.500000.');
  // Zero filtering is lifetime activity, not permission to erase backend money
  // or claim that a positive modeled difference was caused by filtering.
  const noFilter = {...session(0.7109506, 0.7601263), compactions: 0, completed_requests: 40};
  data.sessions = [noFilter];
  data.cost_estimate = total(8, 10, 2, 20, 1);
  await vm.runInContext('refresh()', sandbox);
  assert.match(byClass(firstSession(), 'diagnostics')?.textContent || '',
    /no filtering recorded.*lifetime.*not demonstrated filtering savings/i);
  assert.match(byClass(firstSession(), 'session-savings').textContent, /Estimated difference: \$0\.049176/, 'preserve the valid positive backend difference');
  assert.equal(cards().get('Estimated difference'), '$2.000000 (20.0%)', 'no-filter row must not rewrite backend-owned aggregate');
  delete noFilter.compactions;
  await vm.runInContext('refresh()', sandbox);
  assert.doesNotMatch(byClass(firstSession(), 'diagnostics')?.textContent || '', /no filtering recorded/i, 'unknown filtering activity cannot be asserted to be zero');
  Object.assign(noFilter.shadow, {calls:1, input_tokens: 10000, cached_tokens: 0,
    cache_observation:{calls:1,input_tokens:10000,cached_tokens:0}});
  data.reviewer_cache_mode = 'codex-implicit';
  await vm.runInContext('refresh()', sandbox);
  const reviewerUsage = elements.get('usage').children[1];
  assert.equal(reviewerUsage.children[3].textContent, '0 (provider-reported)');
  assert.match(reviewerUsage.children[3].title || '', /zero.*not.*implicit cach.*disabled/i);
  assert.match(elements.get('config').textContent, /Reviewer cache mode \(configuration\): codex-implicit/);
  noFilter.shadow.cached_tokens = null;
  noFilter.shadow.cache_observation = {calls:0,input_tokens:0,cached_tokens:0};
  await vm.runInContext('refresh()', sandbox);
  assert.equal(elements.get('usage').children[1].children[3].textContent, 'Unavailable', 'missing cache usage must not masquerade as provider-reported zero');
  // Legacy numeric zeros have no observation coverage. A priced cost, or an
  // unavailable cost for another reason, cannot establish cache completeness.
  Object.assign(noFilter.shadow,{calls:2,input_tokens:20000,cached_tokens:700,cost_usd:1,
    unavailable_cost_calls:0,cache_observation:{calls:1,input_tokens:10000,cached_tokens:500}});
  await vm.runInContext('refresh()', sandbox);
  assert.equal(elements.get('usage').children[1].children[3].textContent,
    '500 (known subtotal; 1 of 2 calls)');
  assert.equal(elements.get('usage').children[1].children[4].textContent,'—',
    'partial known cached usage cannot produce an overall percentage');
  delete noFilter.shadow.cache_observation;
  await vm.runInContext('refresh()', sandbox);
  assert.equal(elements.get('usage').children[1].children[3].textContent,'Unavailable',
    'legacy totals cannot retroactively establish provider observations');
  noFilter.shadow.cache_observation={calls:2,input_tokens:20000,cached_tokens:500};
  noFilter.shadow.unavailable_cost_calls=2;
  await vm.runInContext('refresh()', sandbox);
  assert.equal(elements.get('usage').children[1].children[3].textContent,'500 (provider-reported)');
  assert.equal(elements.get('usage').children[1].children[4].textContent,'2.5%',
    'unknown monetary tariffs do not erase independently measured cache usage');
  // Activity is independent of completed accounting. Exercise actual rendering
  // for both pending producers, losses, valid zero and genuinely unavailable.
  for (const pending of ['processing', 'background_review_pending']) {
    for (const [carry, traditional, available, rendered] of [
      [1, 2, true, '$1.000000 (50.0%)'],
      [3, 2, true, '$-1.000000 (-50.0%)'],
      [0, 0, true, '$0.000000'],
      [0, 0, false, 'Unavailable'],
    ]) {
      data.sessions = [{...session(carry, traditional, available), [pending]: true}];
      data.cost_estimate = total(available ? carry : null, available ? traditional : null,
        available ? traditional - carry : null,
        available && traditional ? 100 * (traditional - carry) / traditional : null,
        available ? 1 : 0, available ? 0 : 1);
      await vm.runInContext('refresh()', sandbox);
      assert.equal(cards().get('Estimated difference'), rendered);
      assert.equal(byClass(firstSession(), 'activity').textContent,
        pending === 'processing' ? 'Processing request' : 'Reviewing');
      assert.equal(byClass(firstSession(), 'session-savings').textContent,
        'Estimated difference: ' + rendered + (available && carry > traditional ? ' · higher cost with Carry' : ''));
      assert.match(walk(elements.get('sessions')).map(n=>n.textContent).join('\n'), /in.flight.*not.*included/i);
    }
  }
  for (const [policy, label] of [
    ['legacy_route_cache_v0', /legacy route.cache.*not repaired/i],
    ['mixed_legacy_route_v0_and_model_v1', /mixed cache policies.*retained unchanged.*prospectively/i],
    ['conditional_model_cache_v1', /conditional model cache.*not route entitlement/i],
    ['conditional_model_cache_v2', /conditional model cache v2.*not route entitlement/i],
    ['mixed_versioned_cache_policies', /mixed cache policies.*retained unchanged.*prospectively/i],
  ]) {
    data.sessions = [session(1, 2)];
    data.sessions[0].cost_estimate.counterfactual_cache_policy = policy;
    data.cost_estimate = {...total(1, 2, 1, 50, 1), legacy_cache_policy_sessions: 2,
      mixed_cache_policy_sessions: 3, in_flight_sessions: 1};
    await vm.runInContext('refresh()', sandbox);
    assert.match(walk(firstSession()).map(n=>n.textContent).join('\n'), label);
    assert.match(elements.get('estimate-coverage').textContent, /2 legacy, 3 mixed.*not repaired/i);
    assert.match(elements.get('estimate-coverage').textContent, /in.flight costs.*not included/i);
  }
  // Current projection producer omits old reviewer replay-cleanup counters and
  // has nullable reservation/output fields. Do not invent old architecture.
  data.sessions = [{...session(1, 2), context:{}, planner:{selected:true,
    selected_action:'projection', mode:'unknown', reviewer_estimated_tokens:180000,
    reviewer_required_tokens:null, reviewer_context_tokens:1050000,
    primary_output_allowance_tokens:null}}];
  await vm.runInContext('refresh()', sandbox);
  const current = walk(firstSession()).map(n=>n.textContent).join('\n');
  assert.ok(!/0 reviewer cleanups/.test(current), 'absent replay cleanup count must not be reported as zero');
  assert.ok(!/0 removable|0 protected|0 unreviewed/.test(current), 'absent classifications must not become zero counts');
  assert.match(byClass(firstSession(), 'plan-summary').textContent, /projection.*mode.*not recorded/i, 'unknown plan mode cannot imply an applied or primary rewrite');
  assert.match(current, /planned input.*180k/s, 'known input may be shown even when reservation is unknown');
  assert.match(current, /reservation not recorded/i);
  data.sessions[0].context={removable_items:0,protected_items:0,unreviewed_items:1};
  await vm.runInContext('refresh()', sandbox);
  assert.ok(!byClass(firstSession(), 'unreviewed'), 'mixed-population residual is not a valid unseen-item classification');
  data.sessions[0].completed_requests=1;data.sessions[0].compactions=1;
  await vm.runInContext('refresh()', sandbox);
  assert.equal(byClass(firstSession(), 'request-counts').textContent, '1 completed request · 1 filtered request');
  for(const [selected_action,mode,expected] of [
    ['reviewer_only','audit',/would select reviewer-only cleanup \(audit\)/],
    ['primary','audit',/would select primary compaction \(audit\)/],
    ['projection','compact',/context projection selected/],
    [null,'unknown',/action not recorded.*mode not recorded/],
  ]){
    data.sessions[0].planner={selected:true,forced:true,selected_action,mode};
    await vm.runInContext('refresh()', sandbox);
    assert.match(byClass(firstSession(), 'plan-summary').textContent, expected, 'never infer a different action or executed change from a selection');
  }
  data.sessions[0].planner={candidate:{keep:null,compact:null,savings:null}};
  await vm.runInContext('refresh()', sandbox);
  assert.match(walk(firstSession()).map(n=>n.textContent).join('\n'), /Projected payoff Unavailable.*Keep Unavailable.*Compact Unavailable/, 'missing plan economics are not a zero-percent payoff');
  for(const [keep,compact,savings,expected] of [[2,2,0,'0.0%'],[2,3,-1,'-50.0%'],[0,0,0,'Unavailable']]){
    data.sessions[0].planner.candidate={keep,compact,savings};
    await vm.runInContext('refresh()', sandbox);
    assert.ok(walk(firstSession()).some(n=>n.textContent.startsWith('Projected payoff '+expected)), 'plan economics preserve zero and losses without dividing by zero');
  }
  // Synthetic plan: before/after are the same local tokenizer basis, never receipts.
  data.sessions = [{...session(1, 2), completed_requests:28, compactions:28,
    context:{history_items:753, retained_items:92, removed_items:661, removable_items:661,
      protected_items:69, unreviewed_items:0, memories:10},
    planner:{selected:true, selected_action:'projection', mode:'compact',
      primary_estimated_tokens:864961, primary_retained_estimated_tokens:129381,
      primary_required_tokens:967841.1, primary_retained_required_tokens:158703.1,
      context_margin_percent:10, primary_output_reserve_tokens:16384,
      primary_output_reserve_basis:'configured_capacity_reserve',
      primary_context_tokens:1050000, primary_ordinary_context_tokens:272000}}];
  await vm.runInContext('refresh()', sandbox);
  const projectionSummary = byClass(firstSession(), 'context-summary');
  assert.ok(projectionSummary, 'filtering and capacity explanation has a separate drilldown');
  assert.equal(projectionSummary.tagName,'details','large context explanation starts in a compact drilldown');
  assert.equal(projectionSummary.open,false);
  assert.match(projectionSummary.children[0].textContent,/865k → 129.4k.*local estimate/i,'before/after remains visible when closed');
  assert.ok(byClass(firstSession(),'session-identity'),'compact row groups title and activity');
  const inputBars=byClass(projectionSummary,'input-bars');
  assert.ok(inputBars,'comparable local-token estimates have paired bars');
  assert.deepEqual(walk(inputBars).filter(n=>n.tagName==='progress').map(n=>[n.value,n.max]),[[864961,864961],[129381,864961]]);
  assert.ok(!walk(byClass(firstSession(), 'session-details')).includes(projectionSummary));
  assert.match(noteTexts(projectionSummary).join('\n'), /865k → 129.4k.*local estimate/i);
  assert.equal(byClass(firstSession(), 'request-counts').textContent, '28 completed requests · 28 filtered requests');
  assert.match(noteTexts(projectionSummary).join('\n'), /661 omitted.*92 kept.*last submitted/i);
  assert.match(noteTexts(projectionSummary).join('\n'), /753 source items.*661 eligible to omit.*69 protected.*10 memories/i);
  assert.match(noteTexts(projectionSummary).join('\n'), /same.*items.*repeated.*not.*delet/i);
  assert.ok(!walk(firstSession()).some(n=>n.className==='context-bar'), 'mixed populations must not be stacked as a partition');
  const allowance=noteTexts(projectionSummary).join('\n');
  assert.match(allowance, /Before filtering: 865k input \+ 86.5k margin \(10%\) \+ 16.4k output reserve = 967.8k allowance/);
  assert.match(allowance, /After selected filtering: 129.4k input \+ 12.9k margin \(10%\) \+ 16.4k output reserve = 158.7k allowance/);
  assert.match(allowance, /272k economic total.*includes margin.*reserve.*not.*input target.*hard cap/i);
  assert.match(allowance, /configured.*capacity.*not.*expected output.*usage.*prepaid/i);
  const primaryProgress=walk(projectionSummary).find(n=>n.tagName==='progress');
  assert.equal(primaryProgress?.value,158703.1,'capacity bar describes selected allowance, not the unfiltered history');
  assert.equal(primaryProgress?.max,1050000);
  data.sessions[0].planner.primary_output_reserve_basis='derived_from_saved_allowance_not_client_cap';
  await vm.runInContext('refresh()', sandbox);
  assert.match(noteTexts(byClass(firstSession(),'context-summary')).join('\n'), /derived from.*saved.*not.*client.*cap/i);
  delete data.sessions[0].planner.primary_retained_estimated_tokens;
  delete data.sessions[0].planner.primary_retained_required_tokens;
  delete data.sessions[0].planner.primary_output_reserve_tokens;
  await vm.runInContext('refresh()', sandbox);
  const oldPlan=byClass(firstSession(),'context-summary');
  assert.match(noteTexts(oldPlan).join('\n'), /865k → Unknown/);
  assert.match(noteTexts(oldPlan).join('\n'), /output reserve not recorded/i);
  assert.ok(!walk(oldPlan).some(n=>n.tagName==='progress'),'unknown outgoing allowance cannot use prefilter allowance for utilization');
  // Polling patches keyed rows and disclosures in place, including changed values.
  data.sessions=[{...session(1,2),id:'stable-a',title:'Stable task'},
    {...session(2,4),id:'stable-b',title:'Stable task'}];
  await vm.runInContext('refresh()',sandbox);
  const stableGroup=elements.get('sessions').children[0];
  const stableRow=firstSession(), stableDetails=byClass(stableRow,'session-details');
  const stableSummary=stableDetails.children[0];
  stableGroup.open=true; stableDetails.open=true; stableDetails.scrollTop=73;
  data.sessions.reverse(); data.sessions[1].updated_at=Date.now()/1000;
  data.sessions[1].cost_estimate.carry_cost=1.5;
  await vm.runInContext('refresh()',sandbox);
  assert.equal(elements.get('sessions').children[0],stableGroup,'group identity survives polling');
  assert.equal(firstSession(),stableRow,'session row identity survives changed values and response order');
  assert.equal(byClass(firstSession(),'session-details'),stableDetails,'open drilldown is not remounted');
  assert.equal(stableDetails.children[0],stableSummary,'focused summary keeps its DOM identity');
  assert.equal(stableDetails.open,true); assert.equal(stableGroup.open,true);
  assert.equal(stableDetails.scrollTop,73,'nested scroll position is retained');
  assert.match(byClass(firstSession(),'session-costs').textContent,/With Carry \$1.500000/,'retained rows still update amounts');
  // Arrival/removal of a same-title sibling must not remount or hide an open row.
  const original = {...session(1,2),id:'regroup-a',title:'Regroup task'};
  const sibling = {...session(2,4),id:'regroup-b',title:'Regroup task'};
  data.sessions=[original];
  await vm.runInContext('refresh()',sandbox);
  const regroupRow=firstSession(), regroupContext=byClass(regroupRow,'context-summary');
  const regroupSummary=regroupContext.children[0];
  regroupContext.open=true; regroupContext.scrollTop=41; regroupSummary.focus();
  const assertRegrouped = phase => {
    const liveRows=walk(elements.get('sessions')).filter(n=>n.className==='session');
    assert.equal(liveRows.find(n=>n.id==='history:regroup-a'),regroupRow,phase+': retain article identity');
    assert.equal(byClass(regroupRow,'context-summary'),regroupContext,phase+': retain disclosure identity');
    assert.equal(regroupContext.children[0],regroupSummary,phase+': retain summary identity');
    assert.equal(regroupContext.open,true,phase+': keep open disclosure');
    assert.equal(regroupContext.scrollTop,41,phase+': keep nested scroll');
    assert.equal(document.activeElement,regroupSummary,phase+': retain summary focus');
    for(let ancestor=regroupRow.parentElement;ancestor;ancestor=ancestor.parentElement){
      if(ancestor.tagName==='details')assert.equal(ancestor.open,true,phase+': do not hide row inside a closed group');
    }
    assert.equal(liveRows.length,data.sessions.length,phase+': render each history once');
  };
  data.sessions=[sibling,original];
  await vm.runInContext('refresh()',sandbox);
  assertRegrouped('one to two');
  const regroupGroup=elements.get('sessions').children[0];
  await vm.runInContext('refresh()',sandbox);
  assertRegrouped('unchanged group');
  assert.equal(elements.get('sessions').children[0],regroupGroup,'do not replace the group on ordinary polling');
  regroupGroup.open=false; document.activeElement=null;
  await vm.runInContext('refresh()',sandbox);
  assert.equal(regroupGroup.open,false,'ordinary polling respects an explicitly closed group even with open nested details');
  regroupGroup.open=true; regroupSummary.focus();
  data.sessions=[original];
  await vm.runInContext('refresh()',sandbox);
  assertRegrouped('two to one');
  assert.equal(elements.get('sessions').children[0],regroupRow,'last sibling removal returns the same row to top level');
  document.activeElement=null;
  if(process.env.CARRY_DASHBOARD_STATS_FIXTURE){
    data=JSON.parse(fs.readFileSync(process.env.CARRY_DASHBOARD_STATS_FIXTURE,'utf8'));
    await vm.runInContext('refresh()', sandbox);
    assert.ok(!elements.get('status').textContent.includes('stale'), 'exact frozen producer shape renders without error');
    if(process.env.CARRY_DASHBOARD_CACHE_EXPECT){
      const [cached,percent]=JSON.parse(process.env.CARRY_DASHBOARD_CACHE_EXPECT);
      assert.equal(elements.get('usage').children[0].children[3].textContent,cached);
      assert.equal(elements.get('usage').children[0].children[4].textContent,percent);
    }
    const amount=n=>n==null?'Unavailable':'$'+Number(n).toFixed(6);
    assert.equal(cards().get('With Carry'),amount(data.cost_estimate.carry_cost));
    assert.equal(cards().get('Traditional'),amount(data.cost_estimate.traditional_cost));
    const nativeLedgers = data.sessions.flatMap(s=>[s.primary,s.shadow]);
    const nativeSubtotal = nativeLedgers.some(l=>l.cost_usd!=null)
      ? nativeLedgers.reduce((n,l)=>n+(l.cost_usd||0),0) : null;
    const nativeUnpriced = nativeLedgers.reduce((n,l)=>n+(l.unavailable_cost_calls||0),0);
    assert.equal(cards().get('Observed priced subtotal'),amount(nativeSubtotal));
    if(nativeUnpriced || data.unavailable_sessions){
      assert.match(elements.get('observed-cost-summary').textContent, /known subtotal.*Total unavailable/);
      assert.ok(elements.get('observed-cost-summary').textContent.includes(amount(nativeSubtotal)));
      assert.ok(elements.get('observed-cost-summary').textContent.includes(nativeUnpriced.toLocaleString()+' unpriced call'));
    }
    for(const history of data.sessions.filter(s=>s.compactions===0&&s.cost_estimate?.savings>0)){
      const row = [...walk(elements.get('sessions')),...walk(elements.get('background-sessions'))]
        .find(n=>n.id==='history:'+history.id);
      assert.match(byClass(row,'diagnostics').textContent,/not demonstrated filtering savings/);
    }
    assert.equal(walk(elements.get('sessions')).filter(n=>n.className==='session').length+
      walk(elements.get('background-sessions')).filter(n=>n.className==='session').length,data.sessions.length);
    assert.ok(!/reviewer cleanups/.test(walk(elements.get('sessions')).map(n=>n.textContent).join('\n')));
    console.log('private frozen producer smoke: ok (contents not logged)');
  }
  console.log('proxy dashboard scenarios: ok');
})().catch(error => { console.error(error); process.exitCode = 1; });
