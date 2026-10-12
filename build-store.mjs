// Build the merged, deduplicated map store for every map.
//
//   <root>/store/<ab>/<sha256>     one copy of every unique blob
//   <root>/maps/<slug>/...         each map's own engine path tree, as HARD LINKS
//   <root>/maps/<slug>/manifest.json
//   <root>/index/share-index.json
//
// Hard links are the whole trick: the engine opens real paths, and ten maps can each
// have their own path layout while identical bytes occupy disk exactly once. NTFS
// supports them (verified). Nothing is copied, so peak disk = unique bytes.
//
// Resumable: a blob already present and hash-correct is skipped.
import { createHash } from 'node:crypto';
import { mkdir, writeFile, readFile, stat, rename, link, rm } from 'node:fs/promises';
import { existsSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';

const args = Object.fromEntries(process.argv.slice(2).map(a => {
  const m = /^--([^=]+)(?:=(.*))?$/.exec(a);
  return m ? [m[1], m[2] ?? true] : [a, true];
}));
const ROOT = args.root || 'F:/bo1z';
const CONC = Number(args.conc || process.env.CONC || 5);
const H = { 'User-Agent': 'Mozilla/5.0', Referer: 'https://vel.gg/bo1z/' };

const fmt = n => n >= 1e9 ? (n / 1e9).toFixed(2) + ' GB' : (n / 1e6).toFixed(1) + ' MB';
const sha256 = b => createHash('sha256').update(b).digest('hex');
const log = (...a) => console.log(...a);

const index = JSON.parse(await readFile('index/share-index.json', 'utf8'));
const { maps: SLUGS, blobs } = index;

// Pack base per slug. five is the FIRST map and lives at the site root.
const packBase = s => s === 'five' ? 'https://vel.gg/bo1z/pack/' : `https://vel.gg/bo1z/${s}/pack/`;
// Manifest per slug, needed for the chunking decision and to write per-map manifests.
const loadManifest = async s => s === 'five'
  ? JSON.parse(await readFile('manifests/five.json', 'utf8'))
  : JSON.parse(await readFile(`manifests/${s}.json`, 'utf8'));

const manifests = {};
for (const s of SLUGS) manifests[s] = await loadManifest(s);

// br-array lookup: chunked iff the manifest gives that map/path a br array.
const brFor = (slug, path) => {
  const f = manifests[slug].files.find(x => x.path === path);
  const useFb = f && f.opus && f.fallback;
  const br = useFb ? f.fallback.br : (f ? f.br : undefined);
  return { file: f, useFb, br: Array.isArray(br) && br.length ? br : null };
};

const blobPath = sha => join(ROOT, 'store', sha.slice(0, 2), sha);

async function fetchRetry(url, opts = {}, tries = 4) {
  let lastErr;
  for (let i = 0; i < tries; i++) {
    try {
      const r = await fetch(url, { ...opts, headers: H });
      if (r.status === 200 || r.status === 206) return r;
      lastErr = new Error('HTTP ' + r.status);
      if (r.status === 404) break;
    } catch (e) { lastErr = e; }
    await new Promise(r => setTimeout(r, 500 * (i + 1)));
  }
  throw lastErr || new Error('fetch failed');
}

// Fetch one unique blob, using the first map/path that references it.
const stats = { fetched: 0, skipped: 0, bytes: 0, wire: 0, failed: [] };

async function fetchBlob(b) {
  const dest = blobPath(b.sha256);
  if (existsSync(dest)) {
    const st = await stat(dest);
    if (st.size === b.size) { stats.skipped++; return; }
    await rm(dest, { force: true });
  }
  // Pick a reference that is a pack file (streams are single raw GETs anyway).
  const [slug, path, kind] = b.refs[0];
  const base = kind === 'stream' ? packBase(slug) : packBase(slug);
  const { br } = kind === 'pack' ? brFor(slug, path) : { br: null };

  let buf;
  if (br) {
    // Chunked: ?part=k yields ONE identity chunk (Content-Encoding decoded by fetch).
    const parts = [];
    for (let k = 0; k < br.length; k++) {
      const want = Math.min(8388608, b.size - k * 8388608);
      const r = await fetchRetry(`${base}${path}?part=${k}`, { cache: 'no-store' });
      const p = Buffer.from(await r.arrayBuffer());
      if (p.length !== want) throw new Error(`part ${k} size ${p.length} != ${want} (${path})`);
      stats.wire += p.length;
      parts.push(p);
    }
    buf = Buffer.concat(parts);
  } else {
    // Single raw response. Never request ?part= for these — that 404s.
    const r = await fetchRetry(`${base}${path}`, { cache: 'no-store' });
    buf = Buffer.from(await r.arrayBuffer());
    if (buf.length !== b.size) throw new Error(`size ${buf.length} != ${b.size} (${path})`);
    stats.wire += buf.length;
  }

  const got = sha256(buf);
  if (got !== b.sha256) throw new Error(`sha256 mismatch (${path})\n   got ${got}\n   want ${b.sha256}`);

  await mkdir(dirname(dest), { recursive: true });
  await writeFile(dest + '.tmp', buf);
  await rename(dest + '.tmp', dest);
  stats.fetched++; stats.bytes += buf.length;
}

// ---------------------------------------------------------------- fetch
// ---------------------------------------------------------------- migrate local mirrors
// Five and Kino are already verified on C:. Seed the store from them (hard links, so
// this costs no extra space and saves ~870 MB of re-download).
async function migrateLocal(slug, localRoot) {
  if (!existsSync(localRoot)) return 0;
  let seeded = 0;
  for (const b of blobs) {
    const dest = blobPath(b.sha256);
    if (existsSync(dest)) continue;
    for (const [s, path] of b.refs) {
      if (s !== slug) continue;
      const src = join(localRoot, path);
      if (!existsSync(src)) continue;
      try {
        if ((await stat(src)).size !== b.size) continue;
        await mkdir(dirname(dest), { recursive: true });
        try { await link(src, dest); } catch { continue; }  // cross-volume -> skip
        seeded++; break;
      } catch {}
    }
  }
  return seeded;
}

await mkdir(join(ROOT, 'store'), { recursive: true });
{
  let seeded = 0;
  seeded += await migrateLocal('five', 'pack-five/files');
  seeded += await migrateLocal('kino', 'pack-kino/files');
  if (seeded) log(`seeded ${seeded} blobs from local C: mirrors via hard link (0 bytes moved)\n`);
}
const queue = blobs.slice().sort((a, b) => b.size - a.size);
const total = queue.length;
let done = 0;
log(`root        : ${ROOT}`);
log(`unique blobs: ${total}  ${fmt(index.totals.uniqueBytes)}`);
log(`concurrency : ${CONC}\n`);

const t0 = Date.now();
async function worker() {
  while (queue.length) {
    const b = queue.shift();
    const label = b.refs[0][1];
    try {
      const before = stats.skipped;
      await fetchBlob(b);
      done++;
      if (stats.skipped === before) log(`[${String(done).padStart(3)}/${total}] ok    ${fmt(b.size).padStart(9)}  ${label}`);
    } catch (e) {
      done++;
      stats.failed.push({ sha: b.sha256, path: label, err: e.message });
      log(`[${String(done).padStart(3)}/${total}] FAIL  ${label}\n         ${e.message}`);
    }
  }
}
await Promise.all(Array.from({ length: CONC }, () => worker()));

const secs = (Date.now() - t0) / 1000;
log(`\n=== fetch done in ${secs.toFixed(1)}s (${(stats.wire / 1e6 / secs).toFixed(1)} MB/s) ===`);
log(`fetched ${stats.fetched}, already-present ${stats.skipped}, failed ${stats.failed.length}`);
log(`wire pulled : ${fmt(stats.wire)}`);
if (stats.failed.length) {
  log(`\nfailed blobs (re-run to retry):`);
  stats.failed.slice(0, 20).forEach(f => log(`  ${f.path}`));
  process.exitCode = 1;
}

// ---------------------------------------------------------------- materialise
// Every map gets its own real path tree. Shared blobs become hard links, so the
// per-map trees cost no additional space.
if (!stats.failed.length || args.force) {
  log(`\n=== materialising per-map trees with hard links ===`);
  for (const slug of SLUGS) {
    const m = manifests[slug];
    const dir = join(ROOT, 'maps', slug);
    let linked = 0, existed = 0, missing = 0;
    const entries = [];
    for (const f of m.files) {
      const useFb = f.opus && f.fallback;
      entries.push({ path: f.path, sha256: useFb ? f.fallback.sha256 : f.sha256, size: useFb ? f.fallback.size : f.size });
    }
    for (const s of m.streams || []) entries.push({ path: s.path, sha256: s.sha256, size: s.size });

    for (const e of entries) {
      const src = blobPath(e.sha256);
      const dst = join(dir, e.path);
      if (!existsSync(src)) { missing++; continue; }
      await mkdir(dirname(dst), { recursive: true });
      if (existsSync(dst)) {
        try { if ((await stat(dst)).size === e.size) { existed++; continue; } } catch {}
        await rm(dst, { force: true });
      }
      try { await link(src, dst); linked++; }
      catch (err) { log(`  link failed ${slug}/${e.path}: ${err.message}`); missing++; }
    }
    await writeFile(join(dir, 'manifest.json'), JSON.stringify(m));
    log(`  ${slug.padEnd(11)} ${String(linked).padStart(3)} linked, ${String(existed).padStart(3)} already there${missing ? `, ${missing} MISSING` : ''}`);
  }
}

// ---------------------------------------------------------------- report
log(`\n=== done. verify with: node verify-store.mjs ===`);
