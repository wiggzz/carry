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
  listeners: {},
  addEventListener(name, fn) { (this.listeners[name] ||= []).push(fn); },
  get isConnected() { return [...elements.values()].some(root => root.contains(this)); },
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
    return elements.get(id) || null;
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
// Only delivered IDs exist; absent selectors must not invent fixture nodes.
for (const match of html.matchAll(/\bid="([^"]+)"/g)) {
  const node = element(); node.id = match[1]; elements.set(node.id, node);
}
assert.equal(document.getElementById('absent-fixture-node'), null);
vm.runInContext(html.match(/<script>([\s\S]*?)<\/script>/)[1], sandbox);
const cards = () => new Map([...elements.get('cards').children,...elements.get('cost-cards').children].map(card => card.children.map(item => item.textContent)));
(async () => {
  await new Promise(resolve => setImmediate(resolve));
  const walk = el => [el, ...el.children.flatMap(walk)];
  const byClass = (root, cls) => walk(root).find(n => n.className === cls);
  const text = root => walk(root).map(n => n.textContent).join('\n');
  const refresh = () => vm.runInContext('refresh()', sandbox);
  const tokenValues = row => byClass(row,'token-metrics').children.map(block=>byClass(block,'token-value').textContent);
  data.sessions = [
    {...session(1, 2), id:'session-b', title:'Shared opening message'},
    {...session(3, 4), id:'session-a', title:'Shared opening message'},
  ];
  await refresh();
  assert.equal(elements.get('sessions').children.length, 2,
    'same-title sessions must each have their own always-visible row');
  assert.ok(elements.get('sessions').children.every(n => n.className === 'session'),
    'session rows must not be hidden in title-group disclosures');
  assert.deepEqual(elements.get('sessions').children.map(n => n.id),
    ['history:session-a', 'history:session-b'], 'stable order uses session identity');
  assert.ok(elements.get('sessions').children.every(n =>
    walk(n).some(child => child.tagName === 'h3' && child.textContent === 'Shared opening message')),
    'duplicate titles remain readable descriptors rather than merging sessions');
  const s = data.sessions[1];
  s.client={kind:"agent",agent_name:"/root/measurement_audit"};
  Object.assign(s.primary, {calls:4, cost_usd:7, input_tokens:1000, output_tokens:50,
    cache_observation:{calls:2, input_tokens:600, cached_tokens:300}});
  Object.assign(s.shadow, {calls:1, cost_usd:0.25, input_tokens:500, output_tokens:30});
  s.context = {history_items:200, removed_items:145, retained_items:55,
    removable_items:12, protected_items:24, unreviewed_items:19, memories:4};
  s.last_primary_dispatch = {request_id:'request-a', model:'gpt-6-sol', canonical_input_tokens:1400,
    caller_input_tokens:1100, outgoing_input_tokens:700, receipt_input_tokens:725,
    receipt_output_tokens:42, outcome:'completed'};
  s.last_reviewer_dispatch = {request_id:'review-a', model:'reviewer-only-model', outgoing_input_tokens:500,
    receipt_input_tokens:499, receipt_output_tokens:31, outcome:'completed'};
  s.planner = {primary_estimated_tokens:999999,
    candidates:[{candidate_id:'candidate-a',action:'keep',selection_joint:{keep:3,compact:3,savings:0}}]};
  data.cost_estimate = total(9,12,3,25,1,4);
  await refresh();
  const root = elements.get('sessions').children[0];
  const identity=byClass(root,'session-identity');
  const meta=byClass(identity,'session-meta');
  assert.ok(meta,'primary model is visible in the session metadata');
  assert.equal(meta.textContent,'Agent · measurement_audit · '+s.id.slice(0,8)+' · gpt-6-sol');
  assert.ok(!byClass(identity,'session-model'),'there is no separate model label');
  assert.equal(identity.children[identity.children.indexOf(meta)-1].tagName,'h3',
    'combined metadata sits immediately below the session title');
  assert.ok(!meta.textContent.includes('reviewer-only-model'),'reviewer model is never substituted');
  s.last_primary_dispatch.model='gpt-6-luna';
  await refresh();
  assert.strictEqual(byClass(identity,'session-meta'),meta,'polling preserves the metadata node');
  assert.equal(meta.textContent,'Agent · measurement_audit · '+s.id.slice(0,8)+' · gpt-6-luna',
    'model switches update the combined metadata on refresh');
  const details = byClass(root,'session-details');
  assert.ok(details && !details.open, 'technical detail is available but initially collapsed');
  const context = byClass(root,'context-overview');
  assert.ok(context, 'source state breakdown stays outside technical details');
  for(const phrase of ['145 removed','12 removable','24 protected','19 unreviewed','4 memories']) {
    assert.ok(text(context).includes(phrase), 'context overview includes '+phrase);
  }
  assert.ok(!walk(details).includes(context));
  const bar = byClass(context,'context-bar');
  assert.ok(bar, 'context states have a visual source-item distribution');
  assert.equal(bar.children.length,3,'current context excludes removed history and additive memories');
  const removed = byClass(context,'context-removed');
  assert.ok(removed,'removed history stays visible as a count badge');
  assert.equal(removed.tagName,'span','removed history is a badge, not a chart');
  assert.equal(removed.textContent,'145 removed from context');
  const current=byClass(context,'context-current');
  const currentHeading=byClass(current,'context-current-heading');
  assert.strictEqual(removed.parentElement,currentHeading,'badge sits beside the current context heading');
  assert.match(text(currentHeading),/Current context · 55 items/);
  assert.strictEqual(bar.parentElement,current,'current context gets its own full-width bar');
  assert.equal(removed.children.length,0,'removed count has no decorative bar');
  assert.ok(!byClass(context,'context-removed-bar')&&!byClass(context,'context-break'),
    'removed history is not represented by a broken or scaled segment');
  assert.ok(!/not to scale/i.test(text(context)),'no scale disclaimer is needed for a count badge');
  assert.match(bar.getAttribute('aria-label'),/current context/i);
  const legendKeys = walk(context).filter(n => n.className === 'context-key');
  assert.equal(legendKeys.length,3,'every current-context state has a color legend');
  for (const [index,state] of ['removable','protected','unreviewed'].entries()) {
    assert.equal(legendKeys[index].getAttribute('data-state-label'),state);
    assert.equal(bar.children[index].getAttribute('data-state'),state);
    assert.ok(html.includes(`[data-state="${state}"],[data-state-label="${state}"] { --swatch:`),
      'bar and legend share a CSS color for '+state);
  }
  assert.match(html,/\.context-segment\s*\{[^}]*background:var\(--swatch\)/);
  assert.match(html,/\.context-key::before\s*\{[^}]*background:var\(--swatch\)/);
  assert.ok(html.includes('id="conversations-heading">By Session</h2>'));
  const widths = bar.children.map(n => Number.parseFloat(n.style.width));
  widths.forEach((width,index)=>assert.ok(Math.abs(width-[12,24,19][index]/55*100)<1e-9,
    'bar percentages use current source items, not removed history or memories'));
  const huge = vm.runInContext('contextOverview({history_items:1000055,removed_items:1000000,removable_items:12,protected_items:24,unreviewed_items:19,memories:4})',sandbox);
  assert.deepEqual(byClass(huge,'context-bar').children.map(n=>n.style.width),bar.children.map(n=>n.style.width),
    'growing removed history cannot squeeze current context');
  assert.match(text(byClass(huge,'context-removed')),/1,000,000 removed/,'exact removed counts remain visible');
  const tokens = byClass(root,'token-metrics');
  assert.ok(tokens && !walk(details).includes(tokens), 'key token measurements remain visible');
  for(const value of ['1,400','725','42','1,000','50'])assert.ok(text(tokens).includes(value));
  assert.ok(!text(tokens).includes('999,999'),'saved planner prediction cannot masquerade as last raw dispatch');
  assert.ok(!text(tokens).includes('499'),'reviewer receipt is not the primary last-request receipt');
  assert.deepEqual(tokenValues(root),['1,400','700','725 / 42','1,000 / 50'],
    'raw, sent, last receipt, and session totals have independent authoritative sources');
  assert.match(text(tokens),/estimate/i);
  assert.match(text(tokens),/provider/i);
  assert.match(text(byClass(root,'session-costs')),/\$7\.250000/,
    'session observed cost adds provider-priced primary and reviewer usage');
  assert.match(text(byClass(root,'session-savings')),/\$1\.000000/,
    'session comparison comes from backend, not observed cost recomputation');
  assert.equal(cards().get('Observed cost'),'$7.250000');
  assert.equal(cards().get('Estimated savings'),'$3.000000 (25.0%)',
    'aggregate comparison amount and percentage are backend-owned');
  assert.ok(byClass(details,'candidate-table'), 'planner candidates are a technical drilldown');
  assert.ok(byClass(details,'lane-summary'), 'reviewer and primary receipts retain separate lane detail');
  assert.ok(byClass(details,'transcript-view'), 'retained source is fetched only on demand');
  assert.match(text(byClass(details,'lane-summary')),/Reviewer/);
  const unknown = {...session(0,0,false),id:'unknown', title:'Unknown metrics'};
  data.sessions=[unknown];
  await refresh();
  const unknownRow=elements.get('sessions').children[0];
  assert.equal(byClass(unknownRow,'session-meta').textContent,'Unclassified · unknown',
    'legacy sessions without model metadata do not guess a model or add an empty separator');
  assert.deepEqual(tokenValues(unknownRow),['—','—','— / —','— / —'],
    'each missing dispatch and ledger measurement remains unknown independently');
  assert.ok(!text(byClass(unknownRow,'token-metrics')).includes('NaN'));
  assert.match(text(byClass(unknownRow,'session-savings')),/—/);
  assert.ok(!text(byClass(unknownRow,'context-overview')).includes('0 removed'),
    'absent context counts cannot be invented as zero');
  unknown.primary={calls:0,cost_usd:0,input_tokens:0,output_tokens:0};
  unknown.shadow={calls:0,cost_usd:0,input_tokens:0,output_tokens:0};
  unknown.context={history_items:0,retained_items:0,removed_items:0,removable_items:0,
    protected_items:0,unreviewed_items:0,memories:0};
  unknown.last_primary_dispatch={canonical_input_tokens:0,outgoing_input_tokens:0,receipt_input_tokens:0,
    receipt_output_tokens:0,outcome:'completed'};
  await refresh();
  assert.match(text(byClass(unknownRow,'context-overview')),/0 removed/,'known zero stays zero');
  assert.match(text(byClass(unknownRow,'session-costs')),/\$0\.000000/);
  assert.ok(!text(unknownRow).includes('NaN'),'empty distributions cannot divide by zero');
  assert.equal(byClass(unknownRow,'context-bar').children.length,0,'empty current context has no invented slices');
  assert.equal(byClass(unknownRow,'context-removed').textContent,'0 removed from context','zero removed items remain explicit in the badge');
  const allRemoved=vm.runInContext('contextOverview({history_items:200,removed_items:200,removable_items:0,protected_items:0,unreviewed_items:0,memories:2})',sandbox);
  assert.equal(byClass(allRemoved,'context-bar').children.length,0,'all-removed history has an empty current bar');
  assert.match(text(allRemoved),/Current context · 0 items/);
  assert.ok(!text(allRemoved).includes('NaN'));
  const partial=vm.runInContext('contextOverview({removed_items:100,removable_items:12,protected_items:null,unreviewed_items:3})',sandbox);
  assert.equal(byClass(partial,'context-bar').children.length,0,'partial measurements do not invent a distribution');
  assert.match(byClass(partial,'context-bar').getAttribute('aria-label'),/unavailable/i);

  assert.deepEqual(tokenValues(unknownRow),['0','0','0 / 0','0 / 0'],
    'each explicitly measured zero stays known independently');
  delete unknown.last_primary_dispatch.outgoing_input_tokens;
  await refresh();
  assert.deepEqual(tokenValues(unknownRow),['0','—','0 / 0','0 / 0'],
    'one absent estimate cannot erase other known-zero measurements');
  unknown.last_primary_dispatch={canonical_input_tokens:900,receipt_input_tokens:null,
    receipt_output_tokens:null,outcome:'failed'};
  await refresh();
  assert.deepEqual(tokenValues(unknownRow),['900','—','— / —','0 / 0'],
    'failed dispatch keeps raw estimate but cannot borrow previous receipts or totals');
  unknown.primary.unavailable_cost_calls=2;
  unknown.primary.cost_usd=5;
  await refresh();
  assert.match(text(byClass(unknownRow,'session-costs')),/subtotal/i,
    'partially priced session expenses are not labeled complete cost');
  assert.equal(cards().get('Priced subtotal'),'$5.000000');
  data.unavailable_sessions=1;
  await refresh();
  assert.match(elements.get('observed-cost-summary').textContent,/unreadable/i);
  data.unavailable_sessions=0;
  // Keyed updates preserve open drilldowns, nested scroll, and keyboard focus.
  data.sessions=[s,{...session(2,3),id:'session-z',title:s.title}];
  await refresh();
  const stable=elements.get('sessions').children[0];
  const stableDetails=byClass(stable,'session-details'), summary=stableDetails.children[0];
  stableDetails.open=true; stableDetails.scrollTop=73; summary.focus();
  s.primary.cost_usd=8;
  data.sessions.reverse();
  await refresh();
  assert.equal(elements.get('sessions').children[0],stable,'polling retains row identity');
  assert.equal(byClass(stable,'session-details'),stableDetails);
  assert.equal(stableDetails.children[0],summary);
  assert.equal(stableDetails.open,true);
  assert.equal(stableDetails.scrollTop,73);
  assert.equal(document.activeElement,summary);
  assert.match(text(byClass(stable,'session-costs')),/\$8\.250000/);
  data.sessions.push({...session(0,0),id:'session-c',client:{kind:'background'}});
  await refresh();
  assert.equal(elements.get('sessions').children.length,2);
  assert.equal(elements.get('background-sessions').children.length,1);
  assert.equal(elements.get('background-traffic').hidden,false);
  data.sessions=[];
  await refresh();
  assert.equal(elements.get('background-traffic').hidden,true);
  assert.ok(byClass(elements.get('sessions'),'empty'),'empty dashboard has an explicit state');
  if(process.env.CARRY_DASHBOARD_STATS_FIXTURE){
    data=JSON.parse(fs.readFileSync(process.env.CARRY_DASHBOARD_STATS_FIXTURE,'utf8'));
    await refresh();
    assert.ok(!elements.get('status').textContent.includes('stale'), 'exact frozen producer shape renders');
    if(process.env.CARRY_DASHBOARD_CACHE_EXPECT){
      const [cached,percent]=JSON.parse(process.env.CARRY_DASHBOARD_CACHE_EXPECT);
      assert.equal(elements.get('usage').children[0].children[3].textContent,cached);
      assert.equal(elements.get('usage').children[0].children[4].textContent,percent);
    }
    const amount=n=>n==null?'—':'$'+Number(n).toFixed(6);
    assert.equal(cards().get('With Carry'),amount(data.cost_estimate.carry_cost));
    assert.equal(cards().get('Traditional'),amount(data.cost_estimate.traditional_cost));
    const native=data.sessions.flatMap(s=>[s.primary,s.shadow]);
    const cost=native.some(l=>l.cost_usd!=null)?native.reduce((n,l)=>n+(l.cost_usd||0),0):null;
    const missing=native.some(l=>l.unavailable_cost_calls)||data.unavailable_sessions;
    assert.equal(cards().get(missing?'Priced subtotal':'Observed cost'),amount(cost));
    assert.equal(walk(elements.get('sessions')).filter(n=>n.className==='session').length+
      walk(elements.get('background-sessions')).filter(n=>n.className==='session').length,data.sessions.length);
    console.log('private frozen producer smoke: ok (contents not logged)');
  }
  console.log('proxy dashboard scenarios: ok');
})().then(() => require('./proxy_dashboard_measurements.cjs')).catch(error => { console.error(error); process.exitCode = 1; });
