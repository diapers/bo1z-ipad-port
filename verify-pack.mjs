// Independent verification of the mirrored pack: re-hash every file from disk
// against the manifest, using the variant-correct expected values.
import { createHash } from 'node:crypto';
import { readFileSync, existsSync, statSync } from 'node:fs';
import { join } from 'node:path';

const H = { 'User-Agent': 'Mozilla/5.0', Referer: 'https://vel.gg/bo1z/' };
const M = await (await fetch('https://vel.gg/bo1z/manifest.json', { headers: H })).json();
const ROOT = process.argv[2] || 'pack-five/files';
const fmt = n => n > 1e6 ? (n / 1e6).toFixed(1) + ' MB' : (n / 1e3).toFixed(1) + ' KB';
const sha = b => createHash('sha256').update(b).digest('hex');

let ok = 0, bad = 0, missing = 0, bytes = 0;
const rows = [];
for (const f of M.files) {
  const useFb = f.opus && f.fallback;
  const want = useFb ? f.fallback.sha256 : f.sha256;
  const wantSize = useFb ? f.fallback.size : f.size;
  const p = join(ROOT, f.path);
  if (!existsSync(p)) { missing++; rows.push(['MISSING', f.path, 0]); continue; }
  const buf = readFileSync(p);
  const good = buf.length === wantSize && sha(buf) === want;
  bytes += buf.length;
  if (good) ok++; else bad++;
  rows.push([good ? (useFb ? 'ok(fb)' : 'ok') : 'BAD', f.path, buf.length]);
}

console.log('=== pack verification ===');
console.log(`verified : ${ok}`);
console.log(`bad      : ${bad}`);
console.log(`missing  : ${missing}`);
console.log(`bytes    : ${fmt(bytes)}`);
console.log('\nby class (manifest priority):');
for (const cls of M.classes) {
  const fs = M.files.filter(f => f.priority === cls);
  const tot = fs.reduce((s, f) => s + ((f.opus && f.fallback) ? f.fallback.size : f.size), 0);
  console.log(`  ${cls.padEnd(11)} ${String(fs.length).padStart(2)} files  ${fmt(tot)}`);
}
console.log('\n=== the files that matter for booting Pentagon ===');
for (const name of ['zone/Common/zombie_pentagon.ff', 'zone/english/en_zombie_pentagon.ff',
                    'zone/Common/common_zombie.ff', 'web/shaders/shaders.pack',
                    'web/shaders/shaders-wgsl.pack', 'web/sound/sounds.pack']) {
  const r = rows.find(x => x[1] === name);
  console.log(`  ${r ? r[0].padEnd(8) : 'MISSING '} ${fmt(r ? r[2] : 0).padStart(9)}  ${name}`);
}
console.log('\n=== streams (cinematics, fetched separately) ===');
for (const s of M.streams) console.log(`  ${fmt(s.size).padStart(9)}  ${s.path}`);

if (bad || missing) { console.log('\nINCOMPLETE'); process.exitCode = 1; }
else console.log('\nALL FILES PRESENT AND HASH-VERIFIED.');
