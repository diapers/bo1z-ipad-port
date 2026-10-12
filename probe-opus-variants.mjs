// Resolve: for opus/fallback entries, which variant does the server actually serve,
// and does applying the manifest's own `fallback` fields produce a verified download?
import { createHash } from 'node:crypto';

const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36';
const H = { 'User-Agent': UA, Referer: 'https://vel.gg/bo1z/' };
const M = await (await fetch('https://vel.gg/bo1z/manifest.json', { headers: H })).json();
const CHUNK = M.transport.chunk;
const sha = b => createHash('sha256').update(b).digest('hex');

const opusFiles = M.files.filter(f => f.opus && f.fallback);
console.log(`entries with opus+fallback: ${opusFiles.length}`);
console.log('\npath                                   primary.size  fb.size   primary.br  fb.br');
for (const f of opusFiles) {
  console.log(`${f.path.padEnd(38)} ${String(f.size).padStart(10)}  ${String(f.fallback.size).padStart(8)}  ${String(f.br.length).padStart(9)}  ${String(f.fallback.br.length).padStart(5)}`);
}

// For each, download using the FALLBACK spec (that is what the server appears to serve)
// and verify against BOTH hashes so we learn the ground truth.
console.log('\n=== verify fallback-spec download against both hashes ===');
for (const f of opusFiles) {
  const fb = f.fallback;
  // Fallback chunking: identity chunks of CHUNK over fb.size (same transport rule).
  const parts = Math.ceil(fb.size / CHUNK);
  const bufs = [];
  let ok = true;
  for (let k = 0; k < parts; k++) {
    const want = Math.min(CHUNK, fb.size - k * CHUNK);
    const url = `https://vel.gg/bo1z/pack/${f.path}?part=${k}`;
    try {
      const r = await fetch(url, { headers: H, cache: 'no-store' });
      const b = Buffer.from(await r.arrayBuffer());
      if (b.length !== want) { console.log(`  ${f.path}: part ${k} ${b.length} != ${want} (parts=${parts})`); ok = false; break; }
      bufs.push(b);
    } catch (e) { console.log(`  ${f.path}: ERR ${e.message}`); ok = false; break; }
  }
  if (!ok) continue;
  const all = Buffer.concat(bufs);
  const h = sha(all);
  const verdict = h === fb.sha256 ? 'FALLBACK ✔'
    : h === f.sha256 ? 'PRIMARY ✔'
    : 'NEITHER ✘';
  console.log(`  ${f.path}`);
  console.log(`     bytes=${all.length} (primary ${f.size} / fallback ${fb.size})  ${verdict}`);
  if (verdict === 'NEITHER ✘') console.log(`     got ${h}\n     fb  ${fb.sha256}\n     pri ${f.sha256}`);
}

// Does there exist a way to get the PRIMARY variant? Probe URL shapes on one file.
const probe = opusFiles.sort((a, b) => b.size - a.size)[0];
console.log(`\n=== hunting the PRIMARY variant for "${probe.path}" (size ${probe.size}) ===`);
const shapes = [
  ['?part=0', `https://vel.gg/bo1z/pack/${probe.path}?part=0`],
  ['?part=0&v=', `https://vel.gg/bo1z/pack/${probe.path}?part=0&v=${probe.sha256.slice(0, 16)}`],
  ['raw', `https://vel.gg/bo1z/pack/${probe.path}`],
  ['raw?v=', `https://vel.gg/bo1z/pack/${probe.path}?v=${probe.sha256.slice(0, 16)}`],
  ['.br/0', `https://vel.gg/bo1z/pack/${probe.path}.br/0`],
  ['.dxs?part=0', `https://vel.gg/bo1z/pack/${probe.path}.dxs?part=0`],
  ['.kop?part=0', `https://vel.gg/bo1z/pack/${probe.path}.kop?part=0`],
];
for (const [label, url] of shapes) {
  try {
    const r = await fetch(url, { headers: H, cache: 'no-store' });
    const b = Buffer.from(await r.arrayBuffer());
    console.log(`  ${label.padEnd(14)} HTTP ${r.status} len=${String(b.length).padStart(9)} ce=${r.headers.get('content-encoding')} cl=${r.headers.get('content-length')}`);
  } catch (e) { console.log(`  ${label.padEnd(14)} ERR ${e.message}`); }
}
