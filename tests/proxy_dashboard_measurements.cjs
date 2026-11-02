// Execute the actual delivered dashboard script with the existing DOM harness.
const fs = require('node:fs');
const vm = require('node:vm');
const harness = fs.readFileSync('tests/proxy_dashboard.cjs', 'utf8').split('(async () => {')[0];
async function measurementContract() {
  await new Promise(resolve => setImmediate(resolve));
  const walk = el => [el, ...el.children.flatMap(walk)];
  const byClass = (root, cls) => walk(root).find(n => n.className === cls);
  const s = session(1, 2);
  Object.assign(s.primary, {calls: 4, input_tokens: 1000, output_tokens: 50, cost_usd: 7,
    cache_observation: {calls: 2, input_tokens: 600, cached_tokens: 300}});
  Object.assign(s.shadow, {calls: 1, input_tokens: 500, cost_usd: .25});
  s.rewrites = {dispatched: 2, forced: 1, prior_unknown: true};
  s.last_primary_dispatch = {request_id:'p1', lane:'primary', caller_input_tokens:1100,
    canonical_input_tokens:1400, continuation_input_tokens:1200, outgoing_input_tokens:700,
    receipt_input_tokens:725, outcome:'completed', basis:'local_estimate'};
  s.last_reviewer_dispatch = {request_id:'r1', lane:'reviewer', outgoing_input_tokens:501,
    receipt_input_tokens:500, outcome:'completed', basis:'local_estimate'};
  s.planner = {selected_candidate_id:'smaller', selected:true, forced:true, payoff_requests:3,
    candidates:[{candidate_id:'keep', action:'keep', primary_retained_estimated_tokens:1200, feasible:true,
      selection_joint:{keep:3,compact:3,savings:0}},
      {candidate_id:'smaller', action:'projection', primary_retained_estimated_tokens:700, feasible:true,
      selection_joint:{keep:3,compact:2,savings:1}}]};
  s.last_primary_dispatch.model='previous-primary-model';
  s.last_reviewer_dispatch.model='previous-reviewer-model';
  data = {sessions:[s], unavailable_sessions:0, primary_model:'configured-primary-model',
    classifier_model:'configured-reviewer-model', cost_estimate:total(9,12,3,25,1,4)};
  await vm.runInContext('refresh()', sandbox);
  assert.deepEqual(elements.get('usage').children.map(r=>r.children[0].textContent),
    ['Primary','Reviewer'],'aggregate lane labels must not imply a single model');
  const laneRows=walk(byClass(elements.get('sessions'),'lane-summary')).filter(n=>n.tagName==='tr').slice(1);
  assert.deepEqual(laneRows.map(r=>r.children[0].textContent),['Primary','Reviewer'],
    'session lane labels aggregate models rather than naming the last model');
  const overview = elements.get('cards');
  assert.equal(overview.children.length, 2, 'only actual cost and savings dominate');
  assert.equal(overview.children[0].children[1].textContent, '$7.250000', 'all-usage observed cost, not matched Carry cost');
  assert.equal(overview.children[1].children[1].textContent, '$3.000000 (25.0%)', 'savings are backend-owned');
  assert.equal(overview.children[0].children[0].textContent,'Observed cost');
  assert.equal(elements.get('observed-cost-summary').textContent,'',
    'complete observed cost is not repeated as a prose subtotal');
  assert.deepEqual(elements.get('cost-cards').children.map(c=>c.children[0].textContent),
    ['With Carry','Traditional','Calls excluded from comparison'],
    'comparison details must not repeat observed total or subtotal cards');
  data.unavailable_sessions=1;
  await vm.runInContext('refresh()',sandbox);
  assert.equal(overview.children[0].children[0].textContent,'Observed priced subtotal');
  assert.equal(overview.children[0].children[1].textContent,'$7.250000');
  assert.match(elements.get('observed-cost-summary').textContent,/1 unreadable checkpoint.*Total unavailable/);
  assert.ok(!elements.get('observed-cost-summary').textContent.includes('$'),
    'coverage warning explains incompleteness without repeating money');
  data.unavailable_sessions=0;
  await vm.runInContext('refresh()',sandbox);
  assert.match(elements.get('usage').children[0].children[4].textContent, /50.0%.*known subset.*2.*4/);
  const root = elements.get('sessions').children[0];
  const inputs = byClass(root,'dispatch-inputs');
  assert.ok(inputs, 'dispatch-bound inputs visible outside disclosures');
  assert.equal(walk(inputs).filter(n=>n.tagName==='progress').length,2);
  const bars=walk(inputs).filter(n=>n.tagName==='progress');
  assert.equal(bars[0].max,bars[1].max,'same scale');
  assert.deepEqual(bars.map(n=>n.value),[1100,700]);
  assert.match(byClass(root,'request-counts').textContent,/2 distinct rewrites.*1 forced.*prior unknown/);
  const cacheBar=walk(byClass(root,'lane-summary')).find(n=>n.tagName==='progress');
  assert.ok(cacheBar,'native receipt cache share has its own labeled proportion bar');
  assert.deepEqual([cacheBar.value,cacheBar.max],[300,600]);
  const candidates=byClass(root,'candidate-table');
  assert.ok(candidates && !walk(byClass(root,'session-details')).includes(candidates));
  assert.equal(walk(candidates).filter(n=>n.tagName==='tr').length,3,'header plus every saved candidate');
  assert.ok(walk(candidates).some(n=>/selected.*forced/i.test(n.textContent)));
  const transcript=byClass(root,'transcript-view');
  assert.ok(transcript,'on-demand transcript view');
  const fetched=[]; sandbox.fetch=async (url,options)=>{fetched.push([url,options]);return {ok:true,json:async()=>({canonical_input:[{role:'user',content:'<script>literal only</script>'}],last_primary_wire:null})}};
  transcript.open=true;
  for(const fn of transcript.listeners.toggle)await fn();
  assert.equal(fetched[0][0],'/carry/dashboard/transcript/'+s.id,'transcript must reach the authorized served route from /carry/dashboard');
  assert.equal(fetched[0][1].cache,'no-store');
  assert.match(transcript.children[1].textContent,/<script>literal only<\/script>/,'retained source is inert text');
  sandbox.fetch=async()=>({ok:true,json:async()=>data});
  const second=session(0,0); data.sessions=[second,s];
  const stable=elements.get('sessions').children[0];
  s.last_primary_dispatch.outgoing_input_tokens=650;
  s.planner.candidates[1].selection_joint.savings=-2;
  await vm.runInContext('refresh()', sandbox);
  assert.equal(elements.get('sessions').children[0],stable,'poll order does not remount keyed row');
  assert.ok(walk(byClass(stable,'dispatch-inputs')).some(n=>n.tagName==='progress'&&n.value===650));
  assert.ok(walk(byClass(stable,'candidate-table')).some(n=>n.textContent==='$-2.000000'));
  // The private dashboard boundary applies equally to stats and transcripts.
  // A transcript failure must not add a separate credential workflow.
  data.auth='codex';
  let transcriptStatus=401;
  const authRequests=[];
  sandbox.fetch=async (url,options)=>{
    authRequests.push([url,options]);
    if(url==='/carry/dashboard/stats')return {ok:true,json:async()=>data};
    return {ok:transcriptStatus===200,status:transcriptStatus,
      json:async()=>({canonical_input:[{role:'user',content:'authorized retained source'}]})};
  };
  const toggleTranscript=async()=>{
    transcript.open=false;
    for(const fn of transcript.listeners.toggle)await fn();
    transcript.open=true;
    for(const fn of transcript.listeners.toggle)await fn();
  };

  await toggleTranscript();
  assert.match(transcript.children[1].textContent,/HTTP 401/);
  assert.equal(elements.get('login').hidden,true,'transcript errors do not add a separate login');
  for(let i=0;i<2;i++)await vm.runInContext('refresh()',sandbox);
  assert.equal(elements.get('login').hidden,true,'readable stats keep the private dashboard login-free');
  assert.match(transcript.children[1].textContent,/HTTP 401/,'polling preserves unavailable state instead of resurrecting an older source');
  transcriptStatus=200;
  await toggleTranscript();
  assert.equal(authRequests.at(-1)[1].headers.Authorization,undefined,'private transcript access adds no bearer');
  assert.equal(elements.get('login').hidden,true,'private transcript success needs no extra login');
  assert.match(transcript.children[1].textContent,/authorized retained source/);
  await vm.runInContext('refresh()',sandbox);
  assert.match(transcript.children[1].textContent,/authorized retained source/);
  transcriptStatus=404;
  await toggleTranscript();
  await vm.runInContext('refresh()',sandbox);
  assert.equal(elements.get('login').hidden,true,'unavailable relay transcripts are not misreported as an authentication failure');
  assert.match(transcript.children[1].textContent,/HTTP 404/,'unavailable transcripts must not resurrect cached source on a stats refresh');
  // Newer unavailable results and session removal must fence older successes.
  const waiting=[];
  sandbox.fetch=(url)=>url==='/carry/dashboard/stats'
    ? Promise.resolve({ok:true,json:async()=>data})
    : new Promise(resolve=>waiting.push(resolve));
  const older=toggleTranscript();
  await new Promise(resolve=>setImmediate(resolve));
  const newer=toggleTranscript();
  await new Promise(resolve=>setImmediate(resolve));
  waiting[1]({ok:false,status:404});
  await newer;
  waiting[0]({ok:true,json:async()=>({canonical_input:['obsolete success']})});
  await older;
  assert.match(transcript.children[1].textContent,/HTTP 404/,'older success cannot replace a newer unavailable result');
  const removed=toggleTranscript();
  await new Promise(resolve=>setImmediate(resolve));
  data.sessions=[];
  await vm.runInContext('refresh()',sandbox);
  waiting[2]({ok:true,json:async()=>({canonical_input:['removed session source']})});
  await removed;
  assert.equal(vm.runInContext('transcriptText.has('+JSON.stringify(s.id)+')',sandbox),false,'removed session source must be evicted and cannot return through an in-flight fetch');
  console.log('dashboard measurement contract passed');
}
vm.runInNewContext(harness + '\n(' + measurementContract.toString() + ')().catch(error => {console.error(error);process.exitCode=1});', {require, console, setImmediate, process, URLSearchParams}, {filename:'dashboard-measurement-harness.cjs'});
