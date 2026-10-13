/* Direct S3 downloads: stream packs, validate logical sizes/digests, publish after validation. */
class CourierPackReader {
  constructor(plan, consume) {
    this.plan = plan; this.consume = consume; this.pending = new Uint8Array();
    this.total = 0; this.bodyTotal = 0;
    this.maximumDecoded = 12 + plan.pack_original_bytes + plan.pack_member_count * (1024 * 1024 + 12);
    this.phase = 'magic'; this.remaining = 0; this.index = 0; this.found = false;
  }
  push(chunk) {
    this.total += chunk.length;
    if (this.total > this.maximumDecoded) throw new Error('Pack exceeds its manifest decoding bound.');
    let offset = 0;
    while (offset < chunk.length) {
      if (this.phase === 'done') throw new Error('Pack contains trailing data.');
      if (this.phase === 'body') {
        const count = Math.min(this.remaining, chunk.length - offset);
        if (this.selected) this.consume(chunk.slice(offset, offset + count));
        offset += count; this.remaining -= count;
        if (!this.remaining) { this.phase = 'header-size'; this.index++; }
        continue;
      }
      const required = ({ magic: 8, 'header-size': 4, header: this.headerSize, size: 8 })[this.phase];
      const count = Math.min(required - this.pending.length, chunk.length - offset);
      const buffer = new Uint8Array(this.pending.length + count);
      buffer.set(this.pending); buffer.set(chunk.subarray(offset, offset + count), this.pending.length);
      this.pending = buffer; offset += count;
      if (buffer.length !== required) continue;
      this.pending = new Uint8Array();
      if (this.phase === 'magic') {
        if (new TextDecoder().decode(buffer) !== 'ISCPACK1') throw new Error('Invalid pack magic.');
        this.phase = 'header-size';
      } else if (this.phase === 'header-size') {
        this.headerSize = new DataView(buffer.buffer).getUint32(0, true);
        if (this.headerSize > 1024 * 1024) throw new Error('Pack header too large.');
        if (this.headerSize && this.index >= this.plan.pack_member_count) throw new Error('Pack has too many members.');
        this.phase = this.headerSize ? 'header' : 'done';
      } else if (this.phase === 'header') {
        this.header = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(buffer));
        if (!this.header || typeof this.header !== 'object' || Object.keys(this.header).sort().join(',') !== 'digest,digest_algorithm,path,size') throw new Error('Invalid pack header.');
        this.phase = 'size';
      } else {
        const size = new DataView(buffer.buffer).getBigUint64(0, true);
        if (size > BigInt(Number.MAX_SAFE_INTEGER) || Number(size) !== this.header.size) throw new Error('Invalid pack member size.');
        this.bodyTotal += Number(size);
        if (this.bodyTotal > this.plan.pack_original_bytes) throw new Error('Pack exceeds manifest logical size.');
        this.selected = this.index === this.plan.member_index;
        if (this.selected) {
          const h = this.header;
          if (h.path !== this.plan.relative_path || h.size !== this.plan.original_size || h.digest_algorithm !== this.plan.hash_algorithm || h.digest !== this.plan.digest) throw new Error('Pack member does not match the manifest.');
          this.found = true;
        }
        this.remaining = Number(size);
        this.phase = this.remaining ? 'body' : 'header-size';
        if (!this.remaining) this.index++;
      }
    }
  }
  finish() {
    if (this.phase !== 'done' || this.pending.length || !this.found || this.index !== this.plan.pack_member_count || this.bodyTotal !== this.plan.pack_original_bytes) throw new Error('Pack ended early or selected file was missing.');
  }
}

// Validate the single Courier Zstd frame before the decoder can allocate its window.
class CourierZstdGuard {
  constructor(maximumDecoded) { this.maximumDecoded = maximumDecoded; this.phase = 'header'; this.pending = []; this.headerReady = false; this.remaining = 0; }
  push(chunk) {
    for (let offset = 0; offset < chunk.length;) {
      if (this.phase === 'done') throw new Error('Trailing or concatenated Zstd frames are unsupported.');
      if (this.phase === 'body' || this.phase === 'checksum') {
        const count = Math.min(this.remaining, chunk.length - offset); offset += count; this.remaining -= count;
        if (!this.remaining) this.phase = this.phase === 'checksum' ? 'done' : this.last ? (this.checksum ? 'checksum' : 'done') : 'block';
        if (this.phase === 'checksum' && !this.remaining) this.remaining = 4;
        continue;
      }
      this.pending.push(chunk[offset++]);
      const b = this.pending;
      if (this.phase === 'header') {
        if (b.length < 5) continue;
        if (b[0] !== 0x28 || b[1] !== 0xb5 || b[2] !== 0x2f || b[3] !== 0xfd || (b[4] & 0x1b)) throw new Error('Unsupported Zstd header or dictionary.');
        const single = Boolean(b[4] & 32); const flag = b[4] >> 6;
        const sizeBytes = flag ? 2 ** flag : single ? 1 : 0;
        const sizeOffset = single ? 5 : 6;
        if (b.length < sizeOffset + sizeBytes) continue;
        let size = 0n; for (let j = 0; j < sizeBytes; j++) size += BigInt(b[sizeOffset + j]) << BigInt(j * 8);
        if (flag === 1) size += 256n;
        const base = !single ? 2 ** (10 + (b[5] >> 3)) : 0;
        const window = single ? size : BigInt(base + (base / 8) * (b[5] & 7));
        if (window > 32n * 1024n * 1024n || (sizeBytes && size > BigInt(this.maximumDecoded))) throw new Error('Zstd frame exceeds browser memory or manifest bounds. Use Courier retrieval.');
        this.checksum = Boolean(b[4] & 4); this.headerReady = true; this.phase = 'block'; this.pending = [];
      } else if (b.length === 3) {
        const header = b[0] | (b[1] << 8) | (b[2] << 16); const type = (header >> 1) & 3; const length = header >>> 3;
        if (type === 3 || length > 131072) throw new Error('Invalid Zstd block.');
        this.last = Boolean(header & 1); this.remaining = type === 1 ? 1 : length; this.pending = [];
        this.phase = this.remaining ? 'body' : this.last ? (this.checksum ? 'checksum' : 'done') : 'block';
        if (this.phase === 'checksum') this.remaining = 4;
      }
    }
  }
  finish() { if (this.phase !== 'done') throw new Error('Zstd frame ended early.'); }
}

async function courierDownload(plan, fileHandle, progress = () => {}) {
  const factories = { sha256: () => hashwasm.createSHA256(), blake3: () => hashwasm.createBLAKE3(), xxhash3: () => hashwasm.createXXHash128() };
  for (const field of ['original_size', 'transport_bytes', 'pack_original_bytes', 'pack_member_count']) {
    if (!Number.isSafeInteger(plan[field]) || plan[field] < 0) throw new Error('File size exceeds browser numeric limits. Use Courier retrieval.');
  }
  if (!factories[plan.hash_algorithm]) throw new Error('Unsupported digest algorithm.');
  if (!fileHandle && plan.original_size > 512 * 1024 * 1024) throw new Error('Files over 512 MiB require a browser with the Save File API, or Courier retrieval.');
  const hasher = await factories[plan.hash_algorithm](); hasher.init();
  const output = fileHandle ? await fileHandle.createWritable() : null;
  const chunks = []; let writes = []; let size = 0; let received = 0;
  const consume = (chunk) => {
    size += chunk.length;
    if (size > plan.original_size) throw new Error('File exceeds its manifest size.');
    hasher.update(chunk);
    if (output) writes.push(chunk); else chunks.push(chunk);
  };
  let reader;
  try {
    const response = await fetch(plan.url, { credentials: 'omit', cache: 'no-store', referrerPolicy: 'no-referrer' });
    if (!response.ok || !response.body) throw new Error(`Storage download failed (${response.status}).`);
    reader = response.body.getReader();
    const packed = plan.kind === 'pack';
    const parser = packed ? new CourierPackReader(plan, consume) : null;
    const guard = packed ? new CourierZstdGuard(parser.maximumDecoded) : null;
    let firstChunks = [];
    const decoder = packed ? new fzstd.Decompress((chunk) => parser.push(chunk)) : null;
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      received += value.length;
      if (received > plan.transport_bytes) throw new Error('Storage object exceeds its manifest size.');
      if (packed) {
        guard.push(value);
        if (firstChunks) { firstChunks.push(value); if (guard.headerReady) { for (const chunk of firstChunks) decoder.push(chunk); firstChunks = null; } }
        else decoder.push(value);
      } else consume(value);
      for (const chunk of writes) await output.write(chunk);
      writes = [];
      progress(received, plan.transport_bytes);
    }
    if (received !== plan.transport_bytes) throw new Error('Storage object size does not match the manifest.');
    if (packed) { guard.finish(); decoder.push(new Uint8Array(), true); parser.finish(); }
    for (const chunk of writes) await output.write(chunk);
    if (size !== plan.original_size || hasher.digest('hex') !== plan.digest) throw new Error('File integrity check failed; download was not published.');
    if (output) await output.close();
    else {
      const blob = new Blob(chunks, { type: 'application/octet-stream' });
      const url = URL.createObjectURL(blob); const link = document.createElement('a');
      link.href = url; link.download = plan.relative_path.split('/').pop(); link.click();
      setTimeout(() => URL.revokeObjectURL(url), 60000);
    }
  } catch (error) {
    if (output) await output.abort().catch(() => {});
    if (error instanceof TypeError) throw new Error('Could not read storage. Check the S3 connection and bucket CORS permission for this console origin.');
    throw error;
  } finally {
    if (reader) await reader.cancel().catch(() => {});
  }
}
if (typeof module !== 'undefined') module.exports = { CourierPackReader, CourierZstdGuard, courierDownload };
