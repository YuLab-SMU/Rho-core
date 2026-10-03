import assert from 'node:assert/strict';
import path from 'node:path';
import {pathToFileURL} from 'node:url';
import {test} from 'node:test';

assert.ok(process.env.RHO_UI_TEST_BUILD, 'Compile the UI SDK and set RHO_UI_TEST_BUILD to its output directory');
const {ViewCloseCooperation} = await import(pathToFileURL(path.join(process.env.RHO_UI_TEST_BUILD, 'plugin-ui/view-close.js')));

function fixture(t, handler, acknowledge = async () => {}) {
  const calls = [];
  let close = {phase: 'open'};
  const port = {view: 'original-view', async request(body) {
    calls.push(structuredClone(body));
    if (body.type === 'observe_lifecycle') return {view: port.view, close: structuredClone(close)};
    if (body.type === 'prepare_close') await acknowledge(body);
    return {};
  }};
  const cooperation = new ViewCloseCooperation(port, handler);
  t.after(() => cooperation.dispose());
  return {calls, cooperation, requested(operation = 'original-close') {close = {phase: 'requested', operation};}, opened() {close = {phase: 'open'};}};
}

test('owner preparation may await declared work and concurrent observers acknowledge only once', async t => {
  let finish, preparations = 0, resumed = 0;
  const work = new Promise(resolve => { finish = resolve; });
  const f = fixture(t, {async prepare() {preparations++; await work;}, resume() {resumed++;}});
  await f.cooperation.start();
  f.requested();
  const first = f.cooperation.observe(), second = f.cooperation.observe();
  assert.equal(first, second);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(preparations, 1);
  assert.ok(!f.calls.some(call => call.type === 'prepare_close'));
  finish();
  await first;
  const acknowledgements = f.calls.filter(call => call.type === 'prepare_close');
  assert.equal(acknowledgements.length, 1);
  assert.equal(acknowledgements[0].operation, 'original-close');
  assert.equal('state_version' in acknowledgements[0], false);
  await f.cooperation.observe();
  assert.equal(preparations, 1);
  f.opened(); await f.cooperation.observe();
  assert.equal(resumed, 1);
  assert.equal(f.cooperation.getSnapshot().preparing, false);
});

test('a failed owner preparation refuses its original close without claiming a save', async t => {
  const f = fixture(t, {async prepare() {throw new Error('保存未确认 Ω'.repeat(1000));}});
  await f.cooperation.start(); f.requested(); await f.cooperation.observe();
  assert.ok(!f.calls.some(call => call.type === 'prepare_close'));
  const refusal = f.calls.find(call => call.type === 'refuse_close');
  assert.equal(refusal.operation, 'original-close');
  assert.ok(Buffer.byteLength(refusal.reason) <= 4096);
  assert.ok(!refusal.reason.includes('\ufffd'));
});

test('disposal during owner preparation cannot acknowledge an ended connection', async t => {
  let finish;
  const work = new Promise(resolve => {finish = resolve;});
  const f = fixture(t, {prepare: () => work});
  await f.cooperation.start(); f.requested();
  const pending = f.cooperation.observe();
  await new Promise(resolve => setImmediate(resolve));
  f.cooperation.dispose(); finish(); await pending;
  assert.ok(!f.calls.some(call => ['prepare_close', 'refuse_close'].includes(call.type)));
});

test('lost close acknowledgement retains the original identity and does not replay preparation', async t => {
  let preparations = 0;
  const f = fixture(t, {async prepare() {preparations++;}}, async () => {throw new Error('response lost');});
  await f.cooperation.start(); f.requested(); await f.cooperation.observe();
  const snapshot = f.cooperation.getSnapshot();
  assert.equal(snapshot.operation, 'original-close');
  assert.match(snapshot.error, /unconfirmed/);
  await f.cooperation.observe();
  assert.equal(preparations, 1);
  assert.equal(f.calls.filter(call => call.type === 'prepare_close').length, 1);
  assert.ok(!f.calls.some(call => call.type === 'refuse_close'));
});
