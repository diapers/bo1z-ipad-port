// Fetch the streamed cinematics and verify them against the manifest.
// play.js streams these with range requests while the pack downloads.
import { createHash } from 'node:crypto';
import { mkdir, writeFile, rename, readFile } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { join, dirname } from 'node:path';

const H = { 'User-Agent': 'Mozilla/5.0', Referer: 'https://vel.gg/bo1z/' };
const OUT = process.argv[2] || 'pack-five/files';
const M = await (await fetch('https://vel.gg/bo1z/manifest.json', { headers: H })).json();
const sha = b => createHash('sha256').update(b).digest('hex');
const fmt = n => n > 1e6 ? (n / 1e6).toFixed(2) + ' MB' : (n / 1e3).toFixed(1) + ' KB';

for (const s of M.streams || []) {
  const dest = join(OUT, s.path);
  if (existsSync(dest)) {
    const b = await readFile(dest);
    if (b.length === s.size && sha(b) === s.sha256) { console.log(`skip  ${s.path} (verified)`); continue; }
  }
  await mkdir(dirname(dest), { recursive: true });
  const url = `https://vel.gg/bo1z/pack/${s.path}`;
  const r = await fetch(url, { headers: H });
  if (!r.ok) { console.log(`FAIL  ${s.path}: HTTP ${r.status}`); continue; }
  const buf = Buffer.from(await r.arrayBuffer());
  const good = buf.length === s.size && sha(buf) === s.sha256;
  if (!good) {
    console.log(`FAIL  ${s.path}: size ${buf.length}/${s.size} sha ${good ? 'ok' : 'MISMATCH'}`);
    continue;
  }
  await writeFile(dest + '.tmp', buf);
  await rename(dest + '.tmp', dest);
  console.log(`ok    ${s.path}  ${fmt(buf.length)}  sha256 verified`);
}
