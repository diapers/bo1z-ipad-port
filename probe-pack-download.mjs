// Compare the TWO manifests and locate a working download path for the pack.
import { createHash } from 'node:crypto';
import { brotliDecompressSync } from 'node:zlib';
import { writeFileSync } from 'node:fs';

const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36';
const H = { 'User-Agent': UA, Referer: 'https://vel.gg/bo1z/' };
const fmt = n => n == null ? 'n/a' : n > 1e6 ? (n / 1e6).toFixed(2) + ' MB' : (n / 1e3).toFixed(1) + ' KB';
const sha = b => createHash('sha256').update(b).digest('hex');

const SOURCES = {
  cdn: 'https://cdn.vel.gg/packs/five/manifest.json',
  worker: 'https://vel.gg/bo1z/manifest.json',
};

const manifests = {};
for (const [k, url] of Object.entries(SOURCES)) {
  try {
    const r = await fetch(url, { headers: H });
    if (!r.ok) { console.log(`${k}: HTTP ${r.status}`); continue; }
    const j = await r.json();
    manifests[k] = j;
    const hasBr = (j.files || []).filter(f => Array.isArray(f.br) && f.br.length).length;
    const wire = (j.files || []).reduce((s, f) => s + (Array.isArray(f.br) && f.br.length ? f.br.reduce((a, b) => a + b, 0) : (f.size || 0)), 0);
    console.log(`=== ${k} === ${url}`);
    console.log(`  files=${j.files?.length} total_bytes=${fmt(j.total_bytes)} wire=${fmt(wire)}`);
    console.log(`  files-with-br=${hasBr} streams=${j.streams?.length} schema=${j.schema} transport=${JSON.stringify(j.transport)}`);
    console.log(`  classes=${JSON.stringify(j.class_bytes)}`);
  } catch (e) { console.log(`${k}: ERR ${e.message}`); }
}

// Prefer whichever manifest carries Brotli parts (that is the one that matters
// for a native port, since it tells us how the bytes are actually chunked).
const pick = ['worker', 'cdn'].find(k => (manifests[k]?.files || []).some(f => Array.isArray(f.br) && f.br.length))
          || Object.keys(manifests)[0];
const M = manifests[pick];
console.log(`\nusing manifest: ${pick}`);
writeFileSync('pack-manifest.json', JSON.stringify(M, null, 1));

// Candidate URL builders for a single file.
const file = M.files.find(f => f.path === 'localization.txt')
          || M.files.slice().sort((a, b) => a.size - b.size).find(f => f.size < 500000);

console.log(`\n=== probing download paths for "${file.path}" (${fmt(file.size)}) ===`);
const candidates = [];
if (Array.isArray(file.br) && file.br.length) {
  candidates.push(['cdn  br part0', `https://cdn.vel.gg/packs/five/pack/${file.path}.br/0`]);
  candidates.push(['wrkr br part0', `https://vel.gg/bo1z/pack/${file.path}.br/0`]);
}
candidates.push(['cdn  raw     ', `https://cdn.vel.gg/packs/five/pack/${file.path}`]);
candidates.push(['wrkr raw     ', `https://vel.gg/bo1z/pack/${file.path}`]);

for (const [label, url] of candidates) {
  try {
    const r = await fetch(url, { headers: { ...H, Range: 'bytes=0-262143' } });
    const buf = Buffer.from(await r.arrayBuffer());
    let note = '';
    let plain = buf;
    try { plain = brotliDecompressSync(buf); note = ' (brotli ok)'; } catch { note = ' (not brotli)'; }
    const full = plain.length === file.size && sha(plain) === file.sha256;
    console.log(`  ${label}  HTTP ${r.status}  ${fmt(buf.length)} -> ${fmt(plain.length)}${note}`);
    if (full) console.log(`         ✔ FULL FILE MATCH: sha256 verifies against manifest`);
    else if (file.size <= plain.length) console.log(`         partial`);
  } catch (e) {
    console.log(`  ${label}  ERR ${e.message}`);
  }
}

// Smallest real payload file, so we can attempt a complete, hash-verified download.
const small = M.files.filter(f => f.size > 1000 && f.size < 300000).sort((a, b) => a.size - b.size)[0];
console.log(`\n=== full download + hash verify: "${small.path}" (${fmt(small.size)}) ===`);
for (const [label, url] of [
  ['cdn  raw', `https://cdn.vel.gg/packs/five/pack/${small.path}`],
  ['wrkr raw', `https://vel.gg/bo1z/pack/${small.path}`],
  ['cdn  br0', `https://cdn.vel.gg/packs/five/pack/${small.path}.br/0`],
  ['wrkr br0', `https://vel.gg/bo1z/pack/${small.path}.br/0`],
]) {
  try {
    const r = await fetch(url, { headers: H });
    if (!r.ok) { console.log(`  ${label}: HTTP ${r.status}`); continue; }
    let buf = Buffer.from(await r.arrayBuffer());
    let plain = buf;
    try { plain = brotliDecompressSync(buf); } catch {}
    const good = sha(plain) === small.sha256;
    console.log(`  ${label}: HTTP ${r.status} ${fmt(buf.length)} -> ${fmt(plain.length)}  sha256 ${good ? 'MATCH ✔' : 'mismatch'}`);
  } catch (e) { console.log(`  ${label}: ERR ${e.message}`); }
}

// Streams (video) — range-requested by the page.
console.log('\n=== streamed cinematics ===');
for (const s of M.streams || []) {
  for (const [label, url] of [
    ['cdn ', `https://cdn.vel.gg/packs/five/pack/${s.path}`],
    ['wrkr', `https://vel.gg/bo1z/pack/${s.path}`],
  ]) {
    try {
      const r = await fetch(url, { headers: { ...H, Range: 'bytes=0-2047' } });
      console.log(`  ${label} HTTP ${r.status} accept-ranges=${r.headers.get('accept-ranges')} len=${r.headers.get('content-range')} ${s.path}`);
    } catch (e) { console.log(`  ${label} ERR ${e.message}`); }
  }
}
