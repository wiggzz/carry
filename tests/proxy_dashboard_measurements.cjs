// Exercise the actual delivered script, including lazy source fetch privacy and races.
const fs = require('node:fs');
const vm = require('node:vm');
const harness = fs.readFileSync('tests/proxy_dashboard.cjs', 'utf8').split('(async () => {')[0];
async function measurementContract() {
  await new Promise(resolve => setImmediate(resolve));
  const walk = el => [el, ...el.children.flatMap(walk)];
  const byClass = (root, cls) => walk(root).find(n => n.className === cls);
  const s = session(1, 2);
  Object.assign(s.primary, {calls:4,input_tokens:1000,output_tokens:50,cost_usd:7,
    cache_observation:{calls:2,input_tokens:600,cached_tokens:300}});
  Object.assign(s.shadow, {calls:1,input_tokens:500,output_tokens:30,cost_usd:.25});
  s.last_primary_dispatch={request_id:'p1',caller_input_tokens:1100,
    canonical_input_tokens:1400,outgoing_input_tokens:700,
    receipt_input_tokens:725,receipt_output_tokens:50,outcome:'completed'};
  s.last_reviewer_dispatch={request_id:'r1',outgoing_input_tokens:501,
    receipt_input_tokens:500,receipt_output_tokens:30,outcome:'completed'};
  s.planner={selected_candidate_id:'smaller',selected:true,forced:true,payoff_requests:3,mode:'unknown',
    candidates:[{candidate_id:'keep',action:'keep',primary_retained_estimated_tokens:1200,
      selection_joint:{keep:3,compact:3,savings:0}},
      {candidate_id:'smaller',action:'projection',primary_retained_estimated_tokens:700,
      selection_joint:{keep:3,compact:2,savings:1}}]};
  data={sessions:[s],unavailable_sessions:0,auth:'codex',cost_estimate:total(9,12,3,25,1,4)};
  const statsRequests=[];
  sandbox.fetch=async(url,options)=>{
    statsRequests.push([url,options]);return {ok:true,json:async()=>data};
  };
  await vm.runInContext('refresh()',sandbox);
  assert.ok(statsRequests.every(([url])=>url==='/carry/dashboard/stats'),
    'initial rendering and polling never fetch retained transcript content');
  assert.equal(statsRequests[0][1].cache,'no-store');
  assert.deepEqual(elements.get('usage').children.map(r=>r.children[0].textContent),
    ['Primary','Reviewer'],'usage labels aggregate models, never imply one configured model');
  assert.match(elements.get('usage').children[0].children[4].textContent,/50.0%.*known subset.*2.*4/,
    'provider cache share is limited to observed receipt population');
  const root=elements.get('sessions').children[0];
  const details=byClass(root,'session-details');
  const candidates=byClass(details,'candidate-table');
  assert.ok(candidates,'saved candidates belong inside collapsed technical details');
  assert.equal(candidates.tagName,'details','planner alternatives are a separate disclosure');
  assert.equal(candidates.open,false,'planner alternatives start collapsed');
  const options=walk(candidates).filter(n=>n.className==='planner-option');
  assert.equal(options.length,2,'technical drilldown preserves saved alternatives');
  assert.equal(options[1].getAttribute('data-selected'),'true','saved selected candidate is highlighted');
  assert.equal(options[0].getAttribute('data-selected'),'false');
  assert.ok(walk(options[1]).some(n=>n.textContent==='Use projected context'));
  assert.ok(walk(options[1]).some(n=>n.textContent==='$2.000000'),'projected cost uses the saved candidate');
  assert.ok(walk(options[1]).some(n=>n.textContent==='$1.000000'),'difference uses the saved planner result');
  assert.ok(!walk(details).some(n=>n.tagName==='table'),'session inspection has no wide tables');
  const lanes=walk(details).filter(n=>n.className==='lane-panel');
  assert.equal(lanes.length,2,'primary and reviewer have bounded independent panels');
  assert.deepEqual(lanes.map(n=>n.getAttribute('aria-label')),['Primary usage','Reviewer usage']);
  const primaryCache=byClass(lanes[0],'cache-panel');
  assert.ok(walk(primaryCache).some(n=>n.textContent==='50.0%'));
  assert.ok(walk(primaryCache).some(n=>/2 \/ 4.*known receipts only/.test(n.textContent)),
    'cache share clearly identifies incomplete receipt coverage');
  const reviewerCache=byClass(lanes[1],'cache-panel');
  assert.ok(walk(reviewerCache).some(n=>n.textContent==='Not reported'));
  assert.ok(!walk(reviewerCache).some(n=>n.tagName==='progress'),
    'missing cache usage never becomes a zero-percent meter');
  assert.match(html,/\.cache-panel progress\s*\{[^}]*display:block;[^}]*max-width:100%/,
    'cache meters are block-level and bounded by their panel');
  assert.match(html,/@media\(max-width:1000px\)[^\n]*\.lane-summary,\.planner-options\{grid-template-columns:minmax\(0,1fr\)/,
    'inspection cards stack on narrower screens');
  const cacheBar=walk(byClass(details,'lane-summary')).find(n=>n.tagName==='progress');
  assert.deepEqual([cacheBar.value,cacheBar.max],[300,600],
    'native cache progress is provider observation, not planned savings');
  assert.ok(!walk(byClass(details,'inspection-overview')).some(n=>n.textContent==='unknown'),
    'absent planner mode is not presented as an unexplained unknown');
  const zeroLanes=vm.runInContext(`laneSummary({primary:{calls:1,input_tokens:100,output_tokens:0,
    cache_observation:{calls:1,input_tokens:100,cached_tokens:0}},shadow:{calls:0}})`,sandbox);
  const zeroCache=byClass(zeroLanes,'cache-panel');
  assert.ok(walk(zeroCache).some(n=>n.textContent==='0.0%'),'reported zero cache is not missing');
  const zeroBar=walk(zeroCache).find(n=>n.tagName==='progress');
  assert.deepEqual([zeroBar.value,zeroBar.max],[0,100]);
  const transcript=byClass(root,'transcript-view');
  assert.ok(transcript,'on-demand transcript view');
  const fetched=[]; sandbox.fetch=async (url,options)=>{fetched.push([url,options]);return {ok:true,json:async()=>({canonical_input:[{role:'user',content:'<script>literal only</script>'}],last_primary_wire:null})}};
  transcript.open=true;
  for(const fn of transcript.listeners.toggle)await fn();
  assert.equal(fetched[0][0],'/carry/dashboard/transcript/'+s.id,'transcript must reach the authorized served route from /carry/dashboard');
  assert.equal(fetched[0][1].cache,'no-store');
  assert.match(transcript.children[1].textContent,/<script>literal only<\/script>/,'retained source is inert text');
  sandbox.fetch=async()=>({ok:true,json:async()=>data});
  await vm.runInContext('refresh()',sandbox);
  assert.equal(byClass(elements.get('sessions').children[0],'transcript-view'),transcript,
    'polling retains transcript node and its fetch state');
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
