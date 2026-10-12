// Exact totals for mirroring all 10 maps, plus how much content is duplicated
// between maps (which decides whether a shared/merged layout could save real space).
import { readFileSync } from 'node:fs';

const SLUGS = ['five','kino','riese','nacht','verruckt','shinonuma','ascension','cotd','shangrila','moon'];
const fmt = n => n >= 1e9 ? (n / 1e9).toFixed(2) + ' GB' : (n / 1e6).toFixed(1) + ' MB';

const maps = {};
for (const s of SLUGS) {
  const m = JSON.parse(readFileSync(`manifests/${s}.json`, 'utf8'));
  const files = m.files.map(f => (f.opus && f.fallback)
    ? { path: f.path, size: f.fallback.size, sha256: f.fallback.sha256,
        br: f.fallback.br, variant: 'fallback' }
    : { path: f.path, size: f.size, sha256: f.sha256, br: f.br, variant: 'primary' });
  maps[s] = { files, streams: m.streams || [] };
}

// ---- per-map totals ----
console.log('slug        map                    files  pack disk   streams   TOTAL disk      wire');
console.log('-'.repeat(94));
let sumDisk = 0, sumWire = 0, sumStreams = 0, sumFiles = 0;
for (const s of SLUGS) {
  const { files, streams } = maps[s];
  const disk = files.reduce((a, f) => a + f.size, 0);
  const wire = files.reduce((a, f) => a + (Array.isArray(f.br) && f.br.length
    ? f.br.reduce((x, y) => x + y, 0) : f.size), 0);
  const st = streams.reduce((a, x) => a + x.size, 0);
  sumDisk += disk; sumWire += wire; sumStreams += st; sumFiles += files.length + streams.length;
  const name = JSON.parse(readFileSync(`manifests/${s}.json`, 'utf8')).map || '';
  console.log(`${s.padEnd(11)} ${name.padEnd(22)} ${String(files.length).padStart(4)}  ${fmt(disk).padStart(9)}  ${fmt(st).padStart(8)}  ${fmt(disk + st).padStart(12)}  ${fmt(wire + st).padStart(9)}`);
}
console.log('-'.repeat(94));
console.log(`${'TOTAL'.padEnd(11)} ${'10 maps'.padEnd(22)} ${String(sumFiles).padStart(4)}  ${''.padStart(9)}  ${fmt(sumStreams).padStart(8)}  ${fmt(sumDisk + sumStreams).padStart(12)}  ${fmt(sumWire + sumStreams).padStart(9)}`);

// ---- duplication analysis ----
// Group every (path) across maps and see which are identical by content hash.
const byPath = new Map();
for (const s of SLUGS) {
  for (const f of maps[s].files) {
    if (!byPath.has(f.path)) byPath.set(f.path, []);
    byPath.get(f.path).push({ slug: s, size: f.size, sha256: f.sha256 });
  }
}
let rawSum = 0, sharedIfDeduped = 0, dupWasted = 0;
const sharedRows = [];
for (const [path, copies] of byPath) {
  const sizes = copies.map(c => c.size);
  rawSum += sizes.reduce((a, b) => a + b, 0);
  const distinct = new Map();
  for (const c of copies) distinct.set(c.sha256, c.size);   // identical bytes counted once
  const deduped = [...distinct.values()].reduce((a, b) => a + b, 0);
  sharedIfDeduped += deduped;
  const waste = sizes.reduce((a, b) => a + b, 0) - deduped;
  if (waste > 0) { dupWasted += waste; sharedRows.push([path, copies.length, distinct.size, waste]); }
}
console.log(`\n=== duplication across the 10 maps ===`);
console.log(`raw sum of all pack files      : ${fmt(rawSum)}`);
console.log(`if identical files stored once : ${fmt(sharedIfDeduped)}`);
console.log(`recoverable by dedup           : ${fmt(dupWasted)}  (${(dupWasted / rawSum * 100).toFixed(1)}%)`);
console.log(`\npaths appearing in more than one map: ${sharedRows.length} of ${byPath.size}`);
console.log('\ntop duplicated paths (wasted bytes per path):');
for (const [p, n, distinct, waste] of sharedRows.sort((a, b) => b[3] - a[3]).slice(0, 14)) {
  console.log(`  ${fmt(waste).padStart(9)}  in ${String(n).padStart(2)} maps, ${distinct} distinct  ${p}`);
}

// ---- what the port would actually need ----
const wasm = readFileSync('bo1z-wasm/KisakBlack-web.wasm').length;
const fullDisk = sumDisk + sumStreams;
console.log(`\n=== bottom line ===`);
console.log(`engine wasm (shared by every map)  : ${fmt(wasm)}`);
console.log(`all 10 maps on disk (as-is layout) : ${fmt(fullDisk)}`);
console.log(`all 10 maps, deduped layout        : ${fmt(fullDisk - dupWasted)}`);
console.log(`network cost to fetch all 10       : ${fmt(sumWire + sumStreams)}`);

// ---- device context ----
console.log(`\n=== iPad Air M1 context ===`);
for (const cap of [64, 128, 256]) {
  console.log(`  ${String(cap).padStart(3)} GB iPad: all 10 maps = ${((fullDisk / 1e9) / cap * 100).toFixed(1)}% of storage`);
}
