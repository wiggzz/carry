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
  focus() { this.focused = true; },
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
const sessionRequests = [];
const sandbox = {
  fetch(url) {
    sessionRequests.push(url);
    return Promise.resolve({ ok: true, json: async () => ({ state: 'waiting', model: 'gpt-6-sol', reasoning_effort: 'medium' }) });
  },
  document,
  window: { addEventListener() {}, innerHeight: 0, scrollTo() {}, scrollY: 0, location: 'http://localhost/' },
  crypto: {
    ids: ['submission-a', 'submission-b', 'submission-c', 'submission-d', 'submission-e'],
    randomUUID() { return this.ids.shift(); },
  },
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
assert.equal(elements.get('#text').focused, true, 'composer should be focused at startup');
(async () => {
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(sessionRequests, ['/api/v1/session']);
  assert.ok(html.includes('id="stat-model"'), 'footer must contain a model field');
  assert.equal(elements.get('#stat-model').textContent, 'gpt-6-sol', 'model should be visible before first message');
  assert.equal(elements.get('#stat-reasoning').textContent, 'medium', 'reasoning should be visible before first message');
  vm.runInContext("event({event:'run_started',data:{cwd:'/tmp',model:'gpt-6-luna',reasoning_effort:'high'}})", sandbox);
  assert.equal(elements.get('#stat-model').textContent, 'gpt-6-luna');
  assert.equal(elements.get('#stat-reasoning').textContent, 'high');
  vm.runInContext("event({event:'session_resumed',data:{model:'gpt-6-sol',reasoning_effort:'low'}})", sandbox);
  assert.equal(elements.get('#stat-model').textContent, 'gpt-6-sol');
  assert.equal(elements.get('#stat-reasoning').textContent, 'low');
})().catch(error => { console.error(error); process.exitCode = 1; });

// Memory notes appear once for accepted memories, including on history replay.
const activity = elements.get('#activity');
const memoryEvent = {run_id:'memory',seq:1,event:'context_signals',data:{signals:{keep:[],drop:[],added:[9]},memories:['  Remember the user prefers short replies  ']}};
vm.runInContext('event('+JSON.stringify(memoryEvent)+');event('+JSON.stringify(memoryEvent)+')',sandbox);
assert.equal(activity.children.filter(el => el.className.includes('memory')).length, 1, 'remembered facts should produce one small note, even after reconnect');
assert.equal(activity.children.find(el => el.className.includes('memory'))?.textContent, 'Remembered · Remember the user prefers short replies');
vm.runInContext("event({event:'context_signals',data:{signals:{added:[]},memories:['   ']}})",sandbox);
assert.equal(activity.children.filter(el => el.className.includes('memory')).length, 1, 'empty memories should not produce notes');

vm.runInContext("event({event:'context_compacted',data:{compaction:{dropped:[9]}}})",sandbox);
const note = activity.children.find(el => el.className.includes('memory'));
assert.equal(note.children.find(el => el.className.includes('removed'))?.textContent, 'removed from agent context', 'removed memory should show its actual retention state');


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
// A response can contain both a free-form assistant message and a finish call.
vm.runInContext("event({event:'assistant_message', data:{message:'Detailed explanation before the call'}});event({event:'turn_finished', data:{answer:'Final answer referring to the explanation'}})", sandbox);
const durableText = elements.get('#activity').children.filter(el => el.className === 'entry carry');
assert.equal(durableText.at(-2).children[0].children[0].children[0].textContent, 'Detailed explanation before the call');
assert.equal(durableText.at(-1).children[0].children[0].children[0].textContent, 'Final answer referring to the explanation');

(async () => {
  let request;
  sandbox.fetch = (_url, options) => {
    request = JSON.parse(options.body);
    return new Promise(() => {});
  };
  vm.runInContext("text.value='please add tests'; send();", sandbox);
  assert.equal(vm.runInContext('pendingMessages.length', sandbox), 1);
  assert.ok(elements.get('#queued')?.children.includes(vm.runInContext('pendingMessages[0].el', sandbox)), 'pending input must stay beside composer, outside timeline');
  assert.equal(vm.runInContext('pendingMessages[0].el.textContent', sandbox), 'You (sending): please add tests');
  assert.equal(request.submission_id, 'submission-a', 'each submission must carry a client-generated identity');
  const draft=vm.runInContext('pendingMessages[0].el', sandbox);
  vm.runInContext("event({event:'human_message',data:{message:'please add tests',submission_id:'submission-a'}})", sandbox);
  assert.equal(draft.removed,true);
  assert.ok(elements.get('#activity').children.some(el=>el.textContent==='You: please add tests'));
  assert.equal(vm.runInContext('pendingMessages.length', sandbox), 0);

  let rejectReplay;
  sandbox.fetch = (_url, options) => {
    request = JSON.parse(options.body);
    return new Promise((_resolve, reject) => { rejectReplay = reject; });
  };
  vm.runInContext("replaying=true; text.value='repeat'; send();", sandbox);
  const replayDraft=vm.runInContext('pendingMessages[0].el', sandbox);
  assert.equal(request.submission_id, 'submission-b');
  vm.runInContext("event({event:'human_message',data:{message:'repeat',submission_id:'historical-submission'}})", sandbox);
  assert.equal(vm.runInContext('pendingMessages.length', sandbox), 1, 'replayed same-text history must not confirm a new submission');
  assert.notEqual(replayDraft.removed, true);
  rejectReplay(new Error('offline'));
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(vm.runInContext('pendingMessages.length', sandbox), 0);
  assert.equal(vm.runInContext('text.value', sandbox), 'repeat', 'a failed POST must restore the unconfirmed draft');
  assert.match(replayDraft.textContent, /^Not confirmed — check before resending:/);

  sandbox.fetch = () => new Promise(() => {});
  vm.runInContext("replaying=true; text.value='race'; send();", sandbox);
  const replayedMatch=vm.runInContext('pendingMessages[0].el', sandbox);
  vm.runInContext("event({event:'human_message',data:{message:'race',submission_id:'submission-c'}})", sandbox);
  assert.equal(vm.runInContext('pendingMessages.length', sandbox), 0, 'an exact identity may confirm a submission captured by the replay snapshot');
  assert.equal(replayedMatch.removed, true);

  const duplicateRequests = [];
  sandbox.fetch = (_url, options) => {
    duplicateRequests.push(JSON.parse(options.body));
    return new Promise(() => {});
  };
  vm.runInContext("replaying=false; text.value='same'; send(); text.value='same'; send();", sandbox);
  assert.deepEqual(duplicateRequests.map(value => value.submission_id), ['submission-d', 'submission-e']);
  const firstDuplicate=vm.runInContext('pendingMessages[0].el', sandbox);
  const secondDuplicate=vm.runInContext('pendingMessages[1].el', sandbox);
  vm.runInContext("event({event:'human_message',data:{message:'same',submission_id:'submission-e'}})", sandbox);
  assert.equal(vm.runInContext('pendingMessages.length', sandbox), 1, 'an acknowledgement must remove only its matching submission');
  assert.equal(vm.runInContext('pendingMessages[0].submissionId', sandbox), 'submission-d');
  assert.notEqual(firstDuplicate.removed, true);
  assert.equal(secondDuplicate.removed, true);
})().catch(error => { console.error(error); process.exitCode = 1; });
vm.runInContext(`
const usageEvent={run_id:'usage',seq:1,event:'model_response',data:{step:4,usage:{input_tokens:1200,cached_input_tokens:800,output_tokens:75,total_tokens:1275}}};
event(usageEvent);event(usageEvent);
`, sandbox);
const usageLines = [...elements.values()].flatMap(el => el.children).filter(el => el.className === 'entry response-usage muted');
assert.equal(usageLines.filter(el => el.textContent === 'Response 4 · 1200 input (800 cached) · 75 output · 1275 total tokens').length, 1, 'per-response usage must be visible and deduplicated on replay');
assert.ok(html.includes('id="stat-usage"'), 'footer should show cumulative usage');
vm.runInContext(`
totals.cost=0;totals.costUnknown=false;
const costEvent={run_id:'cost',seq:1,event:'model_response',data:{usage:{},estimated_cost_usd:0.258}};
event(costEvent);event(costEvent);
`, sandbox);
assert.equal(elements.get('#stat-cost').textContent, '$0.2580');
assert.equal(elements.get('#stat-usage').textContent, '1.2k input (800 cached) 75 output tokens');
vm.runInContext(`event({event:'model_response',data:{usage:{},estimated_cost_usd:null}});`, sandbox);
vm.runInContext('totals.input=2520000;totals.total=2520000;totals.cached=2400000;totals.output=120000;renderStats()', sandbox);
assert.equal(elements.get('#stat-usage').textContent, '2.5m input (2.4m cached) 120k output tokens');
assert.equal(elements.get('#stat-cost').textContent, 'pricing unavailable');
