// Build the content-addressed index for ALL maps.
//
// Pure computation from manifests/<slug>.json — downloads nothing. Produces:
//   index/share-index.json : every (map, path) -> content hash, plus the unique blob list
//
// The merged layout works because the engine opens *paths* but the bytes are what matter:
// store each unique sha256 ONCE, then materialise each map's own path tree with hard
// links. Path layouts stay engine-compatible; identical files cost disk once.
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';

export const SLUGS = ['five','kino','riese','nacht','verruckt','shinonuma','ascension','cotd','shangrila','moon'];

const fmt = n => n >= 1e9 ? (n / 1e9).toFixed(2) + ' GB' : (n / 1e6).toFixed(1) + ' MB';

export function loadMap(slug, dir = 'manifests') {
  const m = JSON.parse(readFileSync(`${dir}/${slug}.json`, 'utf8'));
  // Variant-correct entries: opus/fallback files ship two encodings; `<path>?part=k`
  // serves the FALLBACK, so the fallback fields describe what we actually receive.
  const files = m.files.map(f => (f.opus && f.fallback)
    ? { path: f.path, size: f.fallback.size, sha256: f.fallback.sha256, variant: 'fallback' }
    : { path: f.path, size: f.size, sha256: f.sha256, variant: 'primary' });
  return { meta: m, files, streams: m.streams || [] };
}

const maps = {};
for (const s of SLUGS) maps[s] = loadMap(s);

// ---------- build the index ----------
const blobs = new Map();        // sha256 -> { size, refs: [[slug, path, kind]] }
const addBlob = (sha, size, slug, path, kind) => {
  let b = blobs.get(sha);
  if (!b) { b = { sha256: sha, size, refs: [] }; blobs.set(sha, b); }
  else if (b.size !== size) throw new Error(`size conflict for ${sha}: ${b.size} vs ${size}`);
  b.refs.push([slug, path, kind]);
};

const perMap = {};
let rawBytes = 0, rawCount = 0;
for (const slug of SLUGS) {
  const { files, streams } = maps[slug];
  let disk = 0;
  for (const f of files) { addBlob(f.sha256, f.size, slug, f.path, 'pack'); disk += f.size; rawBytes += f.size; rawCount++; }
  for (const s of streams) { addBlob(s.sha256, s.size, slug, s.path, 'stream'); disk += s.size; rawBytes += s.size; rawCount++; }
  perMap[slug] = { disk, files: files.length, streams: streams.length };
}

let uniqueBytes = 0;
for (const b of blobs.values()) uniqueBytes += b.size;

const shared = [...blobs.values()].filter(b => b.refs.length > 1)
  .sort((a, b) => (b.size * (b.refs.length - 1)) - (a.size * (a.refs.length - 1)));

console.log('=== dedup index ===');
console.log(`maps                 : ${SLUGS.length}`);
console.log(`path references      : ${rawCount}  (${fmt(rawBytes)})`);
console.log(`unique blobs         : ${blobs.size}  (${fmt(uniqueBytes)})`);
console.log(`recoverable by dedup : ${fmt(rawBytes - uniqueBytes)}  (${((rawBytes - uniqueBytes) / rawBytes * 100).toFixed(1)}%)`);
console.log(`shared blobs         : ${shared.length} referenced by >1 map`);

console.log('\n=== how many maps reference each unique blob ===');
const histogram = new Map();
for (const b of blobs.values()) histogram.set(b.refs.length, (histogram.get(b.refs.length) || 0) + 1);
for (const n of [...histogram.keys()].sort((a, b) => b - a)) {
  const inGroup = [...blobs.values()].filter(b => b.refs.length === n);
  const bytes = inGroup.reduce((s, b) => s + b.size, 0);
  const waste = inGroup.reduce((s, b) => s + b.size * (b.refs.length - 1), 0);
  console.log(`  in ${String(n).padStart(2)} maps: ${String(histogram.get(n)).padStart(4)} blobs  ${fmt(bytes).padStart(9)} unique  saves ${fmt(waste).padStart(9)}`);
}

console.log('\n=== top shared blobs ===');
for (const b of shared.slice(0, 12)) {
  console.log(`  ${fmt(b.size).padStart(9)} x${String(b.refs.length).padStart(2)} = ${fmt(b.size * (b.refs.length - 1)).padStart(9)} saved  ${b.refs[0][1]}`);
}

// Same path, DIFFERENT content across maps -> cannot be shared, must not be assumed shared.
const byPath = new Map();
for (const [sha, b] of blobs) for (const [slug, path] of b.refs) {
  if (!byPath.has(path)) byPath.set(path, new Map());
  byPath.get(path).set(sha, (byPath.get(path).get(sha) || 0) + 1);
}
const conflicts = [...byPath.entries()].filter(([, m]) => m.size > 1)
  .sort((a, b) => [...b[1].keys()].length - [...a[1].keys()].length);
console.log(`\n=== same path, DIFFERENT content (${conflicts.length} paths) — must be kept distinct ===`);
for (const [path, m] of conflicts.slice(0, 10)) {
  const sizes = [...m.entries()].map(([sha, n]) => `${n}x${blobs.get(sha).size}B`).join(', ');
  console.log(`  ${path}\n     ${m.size} distinct versions: ${sizes}`);
}

// ---------- write the artifact ----------
const index = {
  generated: new Date().toISOString(),
  maps: SLUGS,
  totals: { rawBytes, uniqueBytes, savedBytes: rawBytes - uniqueBytes, refs: rawCount, blobs: blobs.size },
  perMap,
  blobs: [...blobs.values()].map(b => ({ sha256: b.sha256, size: b.size, refs: b.refs })),
};
mkdirSync('index', { recursive: true });
writeFileSync('index/share-index.json', JSON.stringify(index));
console.log(`\nwrote index/share-index.json (${(JSON.stringify(index).length / 1e6).toFixed(1)} MB)`);
console.log(`\nBOTTOM LINE: ${fmt(rawBytes)} of paths -> ${fmt(uniqueBytes)} of unique bytes`);
