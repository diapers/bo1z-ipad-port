// Verify any mirrored map pack against its own manifest, variant-correct.
// Usage: node verify-map.mjs --map=<slug> [--dir=<pack dir>]
import { createHash } from 'node:crypto';
import { readFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';

const args = Object.fromEntries(process.argv.slice(2).map(a => {
  const m = /^--([^=]+)(?:=(.*))?$/.exec(a);
  return m ? [m[1], m[2] ?? true] : [a, true];
}));
const SLUG = args.map || 'five';
const ROOT = args.dir || `pack-${SLUG}/files`;
const H = { 'User-Agent': 'Mozilla/5.0', Referer: 'https://vel.gg/bo1z/' };

const url = SLUG === 'five'
  ? 'https://vel.gg/bo1z/manifest.json'
  : `https://vel.gg/bo1z/${SLUG}/manifest.json`;
const M = await (await fetch(url, { headers: H })).json();
const fmt = n => n >= 1e9 ? (n / 1e9).toFixed(2) + ' GB' : (n / 1e6).toFixed(1) + ' MB';
const sha = b => createHash('sha256').update(b).digest('hex');

let ok = 0, bad = 0, missing = 0, fb = 0, bytes = 0;
const problems = [];
for (const f of M.files) {
  const useFb = f.opus && f.fallback;
  const want = useFb ? f.fallback.sha256 : f.sha256;
  const wantSize = useFb ? f.fallback.size : f.size;
  const p = join(ROOT, f.path);
  if (!existsSync(p)) { missing++; problems.push(['MISSING', f.path]); continue; }
  const buf = readFileSync(p);
  bytes += buf.length;
  if (buf.length === wantSize && sha(buf) === want) { ok++; if (useFb) fb++; }
  else { bad++; problems.push(['BAD', f.path]); }
}
let sOk = 0, sBad = 0, sBytes = 0;
for (const s of M.streams || []) {
  const p = join(ROOT, s.path);
  if (!existsSync(p)) { problems.push(['MISSING stream', s.path]); continue; }
  const buf = readFileSync(p);
  sBytes += buf.length;
  if (buf.length === s.size && sha(buf) === s.sha256) sOk++; else { sBad++; problems.push(['BAD stream', s.path]); }
}

console.log(`=== ${SLUG} ===`);
console.log(`pack files : ${ok} verified${fb ? ` (${fb} fallback variant)` : ''}, ${bad} bad, ${missing} missing`);
console.log(`cinematics : ${sOk} verified, ${sBad} bad`);
console.log(`on disk    : ${fmt(bytes + sBytes)}  (pack ${fmt(bytes)} + streams ${fmt(sBytes)})`);
console.log(`manifest   : ${M.files.length} files, ${(M.streams || []).length} streams, version ${String(M.version).slice(0, 12)}`);
if (problems.length) { console.log('\nproblems:'); problems.forEach(([k, p]) => console.log(`  ${k}  ${p}`)); process.exitCode = 1; }
else console.log('\nALL FILES PRESENT AND HASH-VERIFIED.');
