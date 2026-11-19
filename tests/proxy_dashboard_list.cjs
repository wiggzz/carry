// Execute the delivered dashboard with synthetic pages; no provider calls.
const fs = require('node:fs');
const vm = require('node:vm');
const harness = fs.readFileSync('tests/proxy_dashboard.cjs', 'utf8').split('(async () => {')[0];
async function listContract() {
  await new Promise(resolve => setImmediate(resolve));
  const rows = () => elements.get('sessions').children.filter(n => n.className === 'session');
  const requests = [];
  data.usage={primary:{},reviewer:{}};
  const newer = {...session(0,0), id:'newer', last_primary_dispatch:{at:200}};
  const older = {...session(0,0), id:'older', client:{kind:'background'}, last_primary_dispatch:{at:100}};
  let first = [newer];
  let failMore = false;
  sandbox.fetch = async url => {
    requests.push(url);
    const after = new URL(url,'http://localhost').searchParams.get('after_id');
    if(after && failMore) return {ok:false,status:503};
    return {ok:true,json:async()=>({...data,sessions:after?[older]:first,
      session_page:{has_more:!after,next_after:after?null:'200:'+newer.id}})};
  };
  const refresh = () => vm.runInContext('refresh()',sandbox);
  const click = async button => { for(const fn of button.listeners.click || []) await fn(); };
  await refresh();
  const more = document.getElementById('load-more-sessions');
  assert.ok(more, 'one Load more control replaces page navigation');
  assert.equal(document.getElementById('first-sessions'),null);
  assert.equal(more.disabled,false);
  const firstRow = rows()[0];
  const detail = document.getElementById('disclosure:session:newer');
  detail.open=true;
  failMore=true;
  await click(more);
  assert.deepEqual(rows().map(n=>n.id),['history:newer'],'failed loading retains current list');
  assert.equal(more.disabled,false,'failed loading is retryable');
  failMore=false;
  await click(more);
  assert.deepEqual(rows().map(n=>n.id),['history:newer','history:older'],
    'older background sessions append to the same chronological list');
  assert.strictEqual(rows()[0],firstRow);
  assert.equal(detail.open,true,'loading more preserves inspection state');
  assert.equal(more.hidden,true,'exhausted list hides Load more');
  const newest = {...session(0,0),id:'newest',last_primary_dispatch:{at:300}};
  newer.last_primary_dispatch.at=250;
  first=[newest,newer];
  const beforePoll=requests.length;
  await refresh();
  assert.equal(new URL(requests[beforePoll],'http://localhost').searchParams.has('after_id'),false,
    'polling continues to discover new activity');
  assert.deepEqual(rows().map(n=>n.id),['history:newest','history:newer','history:older']);
  assert.strictEqual(rows()[1],firstRow);
  assert.equal(detail.open,true);
  assert.equal(more.hidden,true,'polling cannot reopen an exhausted older cursor');
  assert.match(elements.get('session-page-note').textContent,/3 sessions/);
  const missing={...session(0,0),id:'missing-comparison',rewrites:{dispatched:3},
    context:{history_items:198,retained_items:85,removed_items:113},
    cost_estimate:{whole_window:null,coverage:{primary_responses:76,unavailable_quantity_calls:76}}};
  sandbox.fetch=async()=>({ok:true,json:async()=>({...data,sessions:[missing]})});
  await refresh();
  const text=node=>[node.textContent,...node.children.map(text)].join('\n');
  assert.match(text(rows()[0]),/comparison quantities.*76|76.*comparison quantities/,
    'missing comparison quantities are explained beside the unavailable savings');
  assert.doesNotMatch(text(rows()[0]),/actual money\/window incomplete/,
    'unknown comparison quantities must not imply missing paid receipts');
  console.log('dashboard chronological list contract passed');
}
vm.runInNewContext(harness+'\n('+listContract.toString()+')().catch(error=>{console.error(error);process.exitCode=1});',
  {require,console,setImmediate,process,URLSearchParams,URL});
