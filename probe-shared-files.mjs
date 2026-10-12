// Investigate the common_zombie.ff 404 for kino, and determine which files are
// genuinely shared across maps (same path, same sha256) versus per-map.
import { createHash } from 'node:crypto';
import { existsSync, readFileSync } from 'node:fs';
import { join } from 'node:path';

const H = { 'User-Agent': 'Mozilla/5.0', Referer: 'https://vel.gg/bo1z/' };
const sha = b => createHash('sha256').update(b).digest('hex');

const load = async (slug) => {
  const u = slug === 'five' ? 'https://vel.gg/bo1z/manifest.json' : `https://vel.gg/bo1z/${slug}/manifest.json`;
  return (await fetch(u, { headers: H })).json();
};

const five = await load('five');
const kino = await load('kino');

const idx = m => new Map(m.files.map(f => [f.path, f]));
const F = idx(five), K = idx(kino);

// 1. What does kino's manifest say about common_zombie.ff?
const target = 'zone/Common/common_zombie.ff';
for (const [name, m] of [['five', F], ['kino', K]]) {
  const e = m.get(target);
  console.log(`\n${name}.manifest "${target}":`);
  if (!e) { console.log('  NOT LISTED in this manifest'); continue; }
  console.log(`  size=${e.size} priority=${e.priority} class=${e.class} br=${JSON.stringify(e.br)}`);
  console.log(`  sha256=${e.sha256}`);
}

// 2. Is it identical where both list it? (shared file, or per-map content?)
const a = F.get(target), b = K.get(target);
if (a && b) {
  console.log(`\nidentical across maps? ${a.sha256 === b.sha256 ? 'YES — same sha256 AND size ' + (a.size === b.size) : 'NO'}`);
}

// 3. Probe the actual URLs for kino.
console.log('\n=== URL probes for kino ===');
const probes = [
  `https://vel.gg/bo1z/kino/pack/${target}`,
  `https://vel.gg/bo1z/kino/pack/${target}?part=0`,
  `https://vel.gg/bo1z/kino/pack/${target}.br/0`,
  `https://vel.gg/bo1z/kino/${target}`,
  `https://vel.gg/bo1z/pack/${target}`,
  `https://vel.gg/bo1z/pack/${target}?part=0`,
];
for (const u of probes) {
  try {
    const r = await fetch(u, { headers: H, cache: 'no-store' });
    console.log(`  HTTP ${r.status}  len=${r.headers.get('content-length') ?? '-'}  ${u.replace('https://vel.gg/bo1z/', '')}`);
  } catch (e) { console.log(`  ERR ${e.message}  ${u}`); }
}

// 4. Which files are shared across ALL maps? (present in every manifest with same sha)
console.log('\n=== cross-map shared files ===');
const slugs = ['five', 'kino', 'riese', 'moon'];
const maps = {};
for (const s of slugs) maps[s] = idx(await load(s));
const allPaths = new Set(Object.values(maps).flatMap(m => [...m.keys()]));
const shared = [], perMap = [], absent = [];
for (const p of allPaths) {
  const entries = slugs.map(s => maps[s].get(p));
  const present = entries.filter(Boolean);
  if (present.length < slugs.length) { absent.push([p, present.length]); continue; }
  const shas = new Set(present.map(e => e.sha256));
  if (shas.size === 1) shared.push([p, present[0].size]); else perMap.push(p);
}
console.log(`  paths total        : ${allPaths.size}`);
console.log(`  shared (all 4 maps): ${shared.length}`);
console.log(`  per-map content    : ${perMap.length}`);
console.log(`  missing from a map : ${absent.length}`);
console.log('\n  shared files:');
for (const [p, sz] of shared.sort((x, y) => y[1] - x[1])) {
  console.log(`    ${(sz / 1e6).toFixed(1).padStart(8)} MB  ${p}`);
}
if (absent.length) {
  console.log('\n  NOT present in all 4 manifests (path : how many maps have it):');
  for (const [p, n] of absent) console.log(`    ${n}/4  ${p}`);
}

// 5. Can we satisfy kino from the local five mirror?
console.log('\n=== local resolution check ===');
const localFive = join('pack-five', 'files', target);
if (existsSync(localFive)) {
  const buf = readFileSync(localFive);
  const want = K.get(target);
  const ok = buf.length === want.size && sha(buf) === want.sha256;
  console.log(`  pack-five/files/${target} exists, ${(buf.length / 1e6).toFixed(1)} MB`);
  console.log(`  matches kino's requirement: ${ok ? 'YES — safe to reuse' : 'NO — different content!'}`);
} else {
  console.log(`  not present locally`);
}
