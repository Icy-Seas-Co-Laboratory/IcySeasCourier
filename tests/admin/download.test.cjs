const { test } = require('node:test');
const assert = require('node:assert/strict');
const vm = require('node:vm');
const fs = require('node:fs');
const path = require('node:path');
const admin = path.resolve(__dirname, '../../apps/data-registry/data_registry/admin');
const { CourierPackReader, CourierZstdGuard, courierDownload } = require(path.join(admin, 'file-download.js'));
const fzstd = require(path.join(admin, 'vendor/fzstd-0.1.1.js'));
const hashwasm = Object.assign({}, ...['sha256', 'blake3', 'xxhash128'].map(name => require(path.join(admin, `vendor/${name}-4.12.0.js`))));
global.fzstd = fzstd; global.hashwasm = hashwasm;
const fixture = require('./pack-fixture.json');
const packed = Buffer.from(fixture.compressed, 'base64');
const plan = { ...fixture.plan, url: 'https://storage.invalid/object', transport_bytes: packed.length };

test('streams valid packed member across arbitrary boundaries and validates all digests', async () => {
  for (const algorithm of ['sha256', 'blake3', 'xxhash3']) {
    const digest = algorithm === 'sha256' ? await hashwasm.sha256('hello') : algorithm === 'blake3' ? await hashwasm.blake3('hello') : await hashwasm.xxhash128('hello');
    if (algorithm === 'xxhash3') assert.equal(digest, 'b5e9c1ad071b3e7fc779cfaa5e523818');
    if (algorithm === 'blake3') assert.equal(digest, 'ea8f163db38682925e4491c5e58d4bb3506ef8c14eb78a86e908c5624a67200f');
    // Standalone checks exercise every configured digest, including XXH3 canonical byte order.
    global.fetch = async () => new Response('hello');
    let saved = false; const chunks = [];
    const handle = { createWritable: async () => ({ write: async (b) => chunks.push(b), close: async () => { saved = true; }, abort: async () => {} }) };
    await courierDownload({ ...plan, kind: 'file', original_size: 5, transport_bytes: 5, hash_algorithm: algorithm, digest }, handle);
    assert.equal(saved, true); assert.equal(Buffer.concat(chunks).toString(), 'hello');
  }
  global.fetch = async () => new Response(new ReadableStream({ start(c) { for (let i = 0; i < packed.length; i += 3) c.enqueue(packed.subarray(i, i + 3)); c.close(); } }));
  const chunks = []; let saved = false;
  await courierDownload(plan, { createWritable: async () => ({ write: async b => chunks.push(b), close: async () => { saved = true; }, abort: async () => {} }) });
  assert.equal(saved, true); assert.equal(Buffer.concat(chunks).toString(), 'hello');
});
test('aborts publication on digest mismatch and truncated storage', async () => {
  for (const data of ['wrong', 'shorter']) {
    let closed = false; let aborted = false;
    global.fetch = async () => new Response(data);
    await assert.rejects(courierDownload({ ...plan, kind: 'file', transport_bytes: 5 }, { createWritable: async () => ({ write: async () => {}, close: async () => { closed = true; }, abort: async () => { aborted = true; } }) }));
    assert.equal(closed, false); assert.equal(aborted, true);
  }
});
test('rejects oversized Zstd windows before decoding and concatenated frames', () => {
  assert.throws(() => new CourierZstdGuard(1000).push(Uint8Array.from([0x28,0xb5,0x2f,0xfd,0,0xff])), /bounds/);
  const guard = new CourierZstdGuard(10000000); guard.push(packed); guard.finish();
  assert.throws(() => guard.push(packed), /concatenated/);
});
test('bounds unselected pack bodies and member count', () => {
  const decoded = fzstd.decompress(packed);
  assert.throws(() => { const parser = new CourierPackReader({ ...plan, pack_original_bytes: 1 }, () => {}); parser.push(decoded); }, /logical size/);
  assert.throws(() => { const parser = new CourierPackReader({ ...plan, pack_member_count: 1 }, () => {}); parser.push(decoded); }, /too many/);
  const parser = new CourierPackReader(plan, () => {}); parser.push(decoded.subarray(0, decoded.length - 1)); assert.throws(() => parser.finish(), /early/);
});
test('logout discards delayed invitation responses', async () => {
  const source = fs.readFileSync(path.join(admin, 'app.js'), 'utf8');
  let resolve; const element = { value: '', hidden: false, replaceChildren() {}, close() {} };
  const context = vm.createContext({ document: { querySelector: () => element, querySelectorAll: () => [] }, DOMException, fetch: () => new Promise(r => { resolve = r; }) });
  vm.runInContext(source.slice(0, source.indexOf('function markActivity()')), context);
  const result = vm.runInContext("state.token = 'old'; api('/invitations/one')", context);
  vm.runInContext('lockConsole()', context);
  resolve({ status: 200, ok: true, json: async () => ({ invitation_code: 'private' }) });
  await assert.rejects(result, e => e.name === 'AbortError');
});
