// Determine the EXACT byte protocol for the pack, from evidence not assumption.
// Candidates:
//   A  https://vel.gg/bo1z/pack/<path>.br/<k>          (Worker, .br part)
//   B  https://vel.gg/bo1z/pack/<path>?part=<k>        (Worker, query part)
//   C  https://vel.gg/bo1z/pack/<path>.br/<k>?v=<sha16>
//   D  https://vel.gg/bo1z/pack/<path>   (raw) with/without Range
// plus: is the body already decoded (Content-Encoding handled by fetch) or still brotli?
import { brotliDecompressSync } from 'node:zlib';
import { createHash } from 'node:crypto';

const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36';
const H = { 'User-Agent': UA, Referer: 'https://vel.gg/bo1z/' };
const M = await (await fetch('https://vel.gg/bo1z/manifest.json', { headers: H })).json();
const CHUNK = M.transport.chunk;
console.log('transport:', JSON.stringify(M.transport), 'chunk =', CHUNK, '=', CHUNK / 1048576, 'MiB');

// Test 1: a file with MANY br parts, so part boundaries are unambiguous.
const f = M.files.filter(x => Array.isArray(x.br) && x.br.length >= 4).sort((a, b) => b.size - a.size)[0];
console.log('\nfile:', f.path);
console.log('  size:', f.size, ' parts:', f.br.length, ' part sizes:', f.br.join(','));
console.log('  expected parts by chunking:', Math.ceil(f.size / CHUNK), 'x', CHUNK);
console.log('  sha256:', f.sha256);

const probes = [
  ['A .br/0        ', `https://vel.gg/bo1z/pack/${f.path}.br/0`],
  ['B ?part=0      ', `https://vel.gg/bo1z/pack/${f.path}?part=0`],
  ['C .br/0?v=     ', `https://vel.gg/bo1z/pack/${f.path}.br/0?v=${f.sha256.slice(0, 16)}`],
  ['D raw          ', `https://vel.gg/bo1z/pack/${f.path}`],
];

const results = [];
for (const [label, url] of probes) {
  try {
    const r = await fetch(url, { headers: H, cache: 'no-store' });
    const buf = Buffer.from(await r.arrayBuffer());
    let decodedByFetch = true;
    let plain = buf;
    // If the body is still brotli, the first bytes will not look like the identity chunk.
    // Try to detect: a successful brotli decode of an already-decoded buffer usually throws.
    let wasBrotli = false;
    try { const d = brotliDecompressSync(buf); if (d.length > buf.length) { plain = d; wasBrotli = true; } } catch { wasBrotli = false; }
    results.push({ label, url, status: r.status, len: buf.length, te: r.headers.get('content-encoding'), cr: r.headers.get('content-range'), cl: r.headers.get('content-length'), wasBrotli, plainLen: plain.length });
    console.log(`\n${label} HTTP ${r.status}`);
    console.log(`  content-encoding=${r.headers.get('content-encoding')} content-length=${r.headers.get('content-length')} content-range=${r.headers.get('content-range')}`);
    console.log(`  received ${buf.length} bytes; brotli-decoded again? ${wasBrotli} -> plain ${plain.length}`);
    console.log(`  first bytes: ${buf.subarray(0, 16).toString('hex')}`);
    if (buf.length === CHUNK && !wasBrotli) console.log('  => looks like ONE IDENTITY CHUNK of exactly 8 MiB (correct!)');
  } catch (e) {
    console.log(`\n${label} ERR ${e.message}`);
  }
}

// Test 2: full-file reassembly via A/B must hash to the manifest sha256.
console.log('\n=== reassemble the whole file from parts and verify sha256 ===');
for (const [label, urlFn] of [
  ['A .br/<k>  ', k => `https://vel.gg/bo1z/pack/${f.path}.br/${k}`],
  ['B ?part=<k>', k => `https://vel.gg/bo1z/pack/${f.path}?part=${k}`],
]) {
  const n = Math.ceil(f.size / CHUNK);
  const parts = [];
  let ok = true;
  try {
    for (let k = 0; k < n; k++) {
      const r = await fetch(urlFn(k), { headers: H, cache: 'no-store' });
      const buf = Buffer.from(await r.arrayBuffer());
      const expect = Math.min(CHUNK, f.size - k * CHUNK);
      if (buf.length !== expect) { console.log(`  ${label} part ${k}: ${buf.length} != ${expect}  MISMATCH`); ok = false; break; }
      parts.push(buf);
    }
    if (ok) {
      const all = Buffer.concat(parts);
      const h = createHash('sha256').update(all).digest('hex');
      console.log(`  ${label} parts=${n} total=${all.length} sha256 ${h === f.sha256 ? 'MATCH ✔' : 'MISMATCH ✘'}`);
      if (h !== f.sha256) console.log(`     want ${f.sha256}\n     got  ${h}`);
    }
  } catch (e) { console.log(`  ${label} ERR ${e.message}`); }
}

// Test 3: raw Range behaviour for a file WITHOUT br parts (resume path).
const raw = M.files.find(x => !Array.isArray(x.br) || !x.br.length);
if (raw) {
  console.log(`\n=== raw Range behaviour on "${raw.path}" (size ${raw.size}) ===`);
  for (const [label, range] of [['no range', null], ['bytes=0-', 'bytes=0-'], ['bytes=100-', 'bytes=100-']]) {
    try {
      const r = await fetch(`https://vel.gg/bo1z/pack/${raw.path}`, { headers: range ? { ...H, Range: range } : H, cache: 'no-store' });
      const buf = Buffer.from(await r.arrayBuffer());
      console.log(`  ${label.padEnd(10)} HTTP ${r.status} len=${buf.length} content-range=${r.headers.get('content-range')}`);
    } catch (e) { console.log(`  ${label} ERR ${e.message}`); }
  }
}
