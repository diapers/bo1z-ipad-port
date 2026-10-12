// Independently verify the merged store:
//   1. every unique blob hashes to its name
//   2. every map's path tree resolves, with correct sizes
//   3. hard links actually share storage (physical < logical)
//   4. report true physical usage
import { createHash } from 'node:crypto';
import { readFileSync, existsSync, statSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

const ROOT = process.argv[2] || 'E:/bo1z';
const fmt = n => n >= 1e9 ? (n / 1e9).toFixed(2) + ' GB' : (n / 1e6).toFixed(1) + ' MB';
const sha = b => createHash('sha256').update(b).digest('hex');

const index = JSON.parse(readFileSync('index/share-index.json', 'utf8'));
const blobs = index.blobs;

console.log(`root: ${ROOT}\n`);

// ---- 1. blob integrity ----
console.log('=== 1. store/blobs: hash every unique blob ===');
let ok = 0, bad = 0, missing = 0, blobBytes = 0;
const badList = [];
for (const b of blobs) {
  const p = join(ROOT, 'store', b.sha256.slice(0, 2), b.sha256);
  if (!existsSync(p)) { missing++; badList.push(['MISSING', b.sha256, b.refs[0][1]]); continue; }
  const buf = readFileSync(p);
  if (buf.length === b.size && sha(buf) === b.sha256) { ok++; blobBytes += buf.length; }
  else { bad++; badList.push(['BAD', b.sha256, b.refs[0][1]]); }
}
console.log(`  verified ${ok}, bad ${bad}, missing ${missing}, total ${fmt(blobBytes)}`);
if (badList.length) badList.slice(0, 10).forEach(([k, s, p]) => console.log(`  ${k}  ${p}  (${s.slice(0, 12)})`));

// ---- 2. per-map trees ----
console.log('\n=== 2. per-map path trees ===');
let linkOk = 0, linkBad = 0, linkMissing = 0;
const perMap = [];
for (const slug of index.maps) {
  const dir = join(ROOT, 'maps', slug);
  const manifestPath = join(dir, 'manifest.json');
  if (!existsSync(manifestPath)) { console.log(`  ${slug.padEnd(11)} NO MANIFEST`); continue; }
  const m = JSON.parse(readFileSync(manifestPath, 'utf8'));
  let n = 0, bad = 0, miss = 0, logical = 0;
  const entries = [];
  for (const f of m.files) {
    const useFb = f.opus && f.fallback;
    entries.push({ path: f.path, size: useFb ? f.fallback.size : f.size });
  }
  for (const s of m.streams || []) entries.push({ path: s.path, size: s.size });
  for (const e of entries) {
    const p = join(dir, e.path);
    if (!existsSync(p)) { miss++; continue; }
    const st = statSync(p);
    if (st.size === e.size) { n++; logical += st.size; }
    else bad++;
  }
  linkOk += n; linkBad += bad; linkMissing += miss;
  perMap.push({ slug, files: entries.length, ok: n, bad, miss, logical });
  console.log(`  ${slug.padEnd(11)} ${String(n).padStart(3)}/${String(entries.length).padStart(3)} ok${bad ? `, ${bad} BAD` : ''}${miss ? `, ${miss} MISSING` : ''}  logical ${fmt(logical)}`);
}

// ---- 3. hard-link sharing ----
console.log('\n=== 3. hard links actually share storage? ===');
console.log(`  logical bytes across all maps : ${fmt(perMap.reduce((s, m) => s + m.logical, 0))}`);
console.log(`  unique blob bytes             : ${fmt(blobBytes)}`);
const saved = perMap.reduce((s, m) => s + m.logical, 0) - blobBytes;
console.log(`  saved by dedup                : ${fmt(saved)}`);

// Compare link counts: if a file is hard-linked N times, nlink >= N.
let multiLinked = 0, sampled = 0;
for (const b of blobs.filter(x => x.refs.length > 1).slice(0, 12)) {
  const p = join(ROOT, 'store', b.sha256.slice(0, 2), b.sha256);
  if (!existsSync(p)) continue;
  const st = statSync(p);
  sampled++;
  if (st.nlink >= b.refs.length) multiLinked++;
  console.log(`  nlink=${String(st.nlink).padStart(2)} refs=${String(b.refs.length).padStart(2)}  ${fmt(b.size).padStart(9)}  ${b.refs[0][1]}`);
}
console.log(`  ${multiLinked}/${sampled} sampled shared blobs have nlink >= refs (proves sharing)`);

// ---- 4. summary ----
console.log('\n=== 4. summary ===');
console.log(`  unique blobs        : ${blobs.length}  ${fmt(blobBytes)}`);
console.log(`  map path references : ${index.totals.refs}`);
console.log(`  maps                : ${index.maps.length}`);
console.log(`  blob integrity      : ${ok} ok, ${bad} bad, ${missing} missing`);
console.log(`  map links           : ${linkOk} ok, ${linkBad} bad, ${linkMissing} missing`);
const pass = bad === 0 && missing === 0 && linkBad === 0 && linkMissing === 0;
console.log(`\n${pass ? 'ALL VERIFIED' : 'INCOMPLETE - see above'}`);
process.exitCode = pass ? 0 : 1;
