// Mirror a BO1Z map's asset pack from the site's own Worker endpoint.
//
// Usage:  node fetch-map.mjs --map=<slug> [--out=<dir>] [--offline]
//   slugs: five kino riese nacht verruckt shinonuma ascension cotd shangrila moon
//   --offline  read the manifest from manifests/<slug>.json instead of the network
//
// Transport (verified in probe-pack-protocol.mjs):
//   chunked -> GET <packbase><path>?part=<k>  (Content-Encoding: br; fetch DECODES it to
//              exactly one 8 MiB identity chunk — never brotli-decode it again)
//   raw     -> GET <packbase><path> with Range: bytes=N-  (206 + Content-Range)
// Every file is verified against the sha256 the manifest publishes. Resumable: a part
// already on disk is skipped, so re-running after a failure is cheap.
import { createHash } from 'node:crypto';
import { mkdir, writeFile, readFile, stat, rename } from 'node:fs/promises';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';

// ---- args ----
const args = Object.fromEntries(process.argv.slice(2).map(a => {
  const m = /^--([^=]+)(?:=(.*))?$/.exec(a);
  return m ? [m[1], m[2] ?? true] : [a, true];
}));
const SLUG = args.map || 'five';
const OFFLINE = !!args.offline;
const ROOT = args.out || `pack-${SLUG}`;
const OUT = join(ROOT, 'files');
const PARTS = join(ROOT, '.parts');

// five is the exception: it is the FIRST map, served at the site root.
const manifestUrl = SLUG === 'five'
  ? 'https://vel.gg/bo1z/manifest.json'
  : `https://vel.gg/bo1z/${SLUG}/manifest.json`;
const packBase = SLUG === 'five'
  ? 'https://vel.gg/bo1z/pack/'
  : `https://vel.gg/bo1z/${SLUG}/pack/`;

const H = { 'User-Agent': 'Mozilla/5.0', Referer: 'https://vel.gg/bo1z/' };
const CONCURRENCY = Number(process.env.CONC || 4);
const RETRIES = 4;
let CHUNK = 8388608; // replaced by manifest.transport.chunk

const fmt = n => n >= 1e9 ? (n / 1e9).toFixed(2) + ' GB' : n > 1e6 ? (n / 1e6).toFixed(1) + ' MB' : (n / 1e3).toFixed(1) + ' KB';
const sha256 = b => createHash('sha256').update(b).digest('hex');
const log = (...a) => console.log(...a);
const seen = { files: 0, skipped: 0, shared: 0, bytes: 0, wire: 0, failed: [] };

async function fetchRetry(url, opts = {}, tries = RETRIES) {
  let lastErr;
  for (let i = 0; i < tries; i++) {
    try {
      const r = await fetch(url, { ...opts, headers: { ...H, ...(opts.headers || {}) } });
      if (r.status === 200 || r.status === 206) return r;
      lastErr = new Error('HTTP ' + r.status);
      if (r.status === 404) break;
    } catch (e) { lastErr = e; }
    await new Promise(r => setTimeout(r, 400 * (i + 1)));
  }
  throw lastErr || new Error('fetch failed');
}

// Fetch one blob (used for both pack files and streamed cinematics) and verify it.
// Many files are byte-identical across maps (verified: common_zombie.ff 51.3 MB has the
// same sha256 in five and kino; 10+ paths are shared across all four maps probed). If a
// sibling mirror already holds a matching copy, reuse it instead of re-downloading.
// Correctness is never assumed from the path — the sha256 must match the manifest.
const siblings = (() => {
  try {
    return readdirSync('.').filter(d => /^pack-/.test(d) && d !== ROOT && existsSync(join(d, 'files')));
  } catch { return []; }
})();

function trySibling(rel, want, sha) {
  for (const s of siblings) {
    const p = join(s, 'files', rel);
    if (!existsSync(p)) continue;
    try {
      const b = readFileSync(p);
      if (b.length === want && sha256(b) === sha) return { from: s, buf: b };
    } catch {}
  }
  return null;
}

async function fetchWhole(rel, want, sha, { streamed = false } = {}) {
  const dest = join(OUT, rel);
  if (existsSync(dest)) {
    const b = await readFile(dest);
    if (b.length === want && sha256(b) === sha) return { skip: true, size: b.length };
  }
  await mkdir(dirname(dest), { recursive: true });
  const r = await fetchRetry(packBase + rel);
  const buf = Buffer.from(await r.arrayBuffer());
  if (buf.length !== want) throw new Error(`${streamed ? 'stream ' : ''}size ${buf.length} != ${want}`);
  const got = sha256(buf);
  if (got !== sha) throw new Error(`sha256 mismatch\n   got ${got}\n   want ${sha}`);
  await writeFile(dest + '.tmp', buf);
  await rename(dest + '.tmp', dest);
  return { skip: false, size: buf.length };
}

async function downloadFile(f) {
  const dest = join(OUT, f.path);
  if (existsSync(dest)) {
    const buf = await readFile(dest);
    if (buf.length === f.size && sha256(buf) === f.sha256) {
      seen.skipped++; seen.bytes += buf.length;
      return `skip   ${f.path} (verified)`;
    }
  }
  await mkdir(dirname(dest), { recursive: true });

  // Chunking is decided by the PRESENCE OF f.br, NOT by file size.
  // A file with an `br` array is served as identity chunks via ?part=k.
  // A file WITHOUT `br` is a single raw response — and if it happens to exceed
  // CHUNK, requesting ?part=1 would 404. (This bit us on kino's
  // zone/Common/common_zombie.ff: 51.3 MB, no br, so 7 phantom parts.)
  const chunked = Array.isArray(f.br) && f.br.length > 0;
  const partCount = chunked ? f.br.length : 1;
  const partDir = join(PARTS, f.path);
  await mkdir(partDir, { recursive: true });

  const chunks = [];
  for (let i = 0; i < partCount; i++) {
    const pf = join(partDir, String(i));
    // For raw files the whole body is the single part; otherwise one identity chunk.
    const want = chunked ? Math.min(CHUNK, f.size - i * CHUNK) : f.size;
    let buf;
    if (existsSync(pf) && (await stat(pf)).size === want) {
      buf = await readFile(pf);
    } else {
      // For a single-part file, an identical copy in a sibling mirror is just as good.
      const sib = partCount === 1 ? trySibling(f.path, want, f.sha256) : null;
      if (sib) {
        buf = sib.buf;
        seen.shared++;
        await writeFile(pf + '.tmp', buf);
        await rename(pf + '.tmp', pf);
      } else {
        const url = packBase + f.path + (chunked ? `?part=${i}` : '');
        const r = await fetchRetry(url, { cache: 'no-store' });
        buf = Buffer.from(await r.arrayBuffer());
        if (buf.length !== want) throw new Error(`part ${i} size ${buf.length} != ${want}`);
        await writeFile(pf + '.tmp', buf);
        await rename(pf + '.tmp', pf);
      }
    }
    seen.wire += buf.length;
    chunks.push(buf);
  }

  const plain = Buffer.concat(chunks);
  if (plain.length !== f.size) throw new Error(`size ${plain.length} != ${f.size}`);
  const got = sha256(plain);
  if (got !== f.sha256) throw new Error(`sha256 mismatch\n   got ${got}\n   want ${f.sha256}`);

  await writeFile(dest + '.tmp', plain);
  await rename(dest + '.tmp', dest);
  seen.files++; seen.bytes += plain.length;
  return `ok     ${f.path}  ${fmt(plain.length)}`;
}

// ---------------------------------------------------------------- main
log(`map: ${SLUG}`);
let manifest;
if (OFFLINE) {
  const local = join('manifests', `${SLUG}.json`);
  log(`manifest (offline): ${local}`);
  manifest = JSON.parse(await readFile(local, 'utf8'));
} else {
  log(`manifest: ${manifestUrl}`);
  manifest = await (await fetch(manifestUrl, { headers: H })).json();
}
if (!Array.isArray(manifest.files)) throw new Error('manifest has no files[]');
CHUNK = (manifest.transport?.encoding === 'br' && manifest.transport.chunk > 0)
  ? manifest.transport.chunk : 8388608;

// Variant handling, straight from download-worker.js:
//   function useFallback(e){ const {size,sha256,br}=e.fallback; Object.assign(e,{size,sha256,br}); delete e.opus; }
// Entries with `opus` + `fallback` ship TWO encodings. `<path>?part=k` serves the
// FALLBACK; the real primary lives at `.kop?part=k` (Opus) or `.dxs?part=k` (DXT-split).
// The site itself ships the fallback when WebCodecs AudioDecoder is unavailable, so it
// is fully supported — and ~181 MB smaller for sounds.pack. We use it.
// Chunking MUST follow the chosen variant or parts are read at wrong offsets.
const normalize = f => (f.opus && f.fallback)
  ? { ...f, size: f.fallback.size, sha256: f.fallback.sha256, br: f.fallback.br, variant: 'fallback' }
  : { ...f, variant: 'primary' };

const files = manifest.files.map(normalize);
const streams = manifest.streams || [];
const fallbackCount = files.filter(f => f.variant === 'fallback').length;
const totalDisk = files.reduce((s, f) => s + f.size, 0) + streams.reduce((s, x) => s + x.size, 0);
const totalWire = files.reduce((s, f) =>
  s + (Array.isArray(f.br) && f.br.length ? f.br.reduce((a, b) => a + b, 0) : f.size), 0);
log(`files=${files.length} (+${streams.length} cinematics) ${fallbackCount} fallback  disk=${fmt(totalDisk)} wire=${fmt(totalWire)}`);
log(`output: ${OUT}   concurrency: ${CONCURRENCY}\n`);

await mkdir(OUT, { recursive: true });
const t0 = Date.now();

// Largest first: long poles start immediately.
const queue = files.slice().sort((a, b) => b.size - a.size);
let done = 0;
const total = queue.length + streams.length;

async function worker() {
  while (queue.length) {
    const f = queue.shift();
    try {
      const msg = await downloadFile(f);
      done++;
      log(`[${String(done).padStart(2)}/${total}] ${msg}`);
    } catch (e) {
      done++;
      seen.failed.push(f.path);
      log(`[${String(done).padStart(2)}/${total}] FAIL   ${f.path}\n        ${e.message}`);
    }
  }
}
await Promise.all(Array.from({ length: CONCURRENCY }, () => worker()));

// Streamed cinematics (play.js range-requests these while the pack downloads).
for (const s of streams) {
  try {
    const r = await fetchWhole(s.path, s.size, s.sha256, { streamed: true });
    done++;
    seen.bytes += r.size;
    log(`[${String(done).padStart(2)}/${total}] ${r.skip ? 'skip  ' : 'ok    '} ${s.path}  ${fmt(r.size)}`);
  } catch (e) {
    done++;
    seen.failed.push(s.path);
    log(`[${String(done).padStart(2)}/${total}] FAIL   ${s.path}\n        ${e.message}`);
  }
}

const secs = (Date.now() - t0) / 1000;
log(`\n=== done in ${secs.toFixed(1)}s ===`);
log(`downloaded ${seen.files}, skipped ${seen.skipped}, reused-from-sibling ${seen.shared}, failed ${seen.failed.length}`);
log(`bytes on disk: ${fmt(seen.bytes)}   wire pulled: ${fmt(seen.wire)}`);
if (seen.failed.length) {
  log(`\nfailed (re-run to retry; parts are cached):`);
  seen.failed.forEach(p => log('  ' + p));
  process.exitCode = 1;
} else {
  log('\nALL FILES VERIFIED against manifest sha256.');
}
