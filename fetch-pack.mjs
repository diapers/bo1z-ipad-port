// Mirror the full BO1Z asset pack from the site's own Worker endpoint.
//
// Transport (from the manifest, verified in probe-pack-protocol.mjs): files are split
// into identity chunks of manifest.transport.chunk (8 MiB) and served as
//   * chunked -> GET pack/<path>?part=<k>   (Content-Encoding: br; fetch decodes it to
//                exactly one 8 MiB identity chunk — never brotli-decode it again)
//   * raw     -> GET pack/<path> with Range: bytes=N-   (206 + Content-Range)
// Every file is verified against the sha256 the manifest publishes.
//
// Resumable: a part already on disk is skipped. Safe to re-run after a failure.
import { createHash } from 'node:crypto';
import { mkdir, writeFile, readFile, stat, rename } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { dirname, join } from 'node:path';

const UA = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/120 Safari/537.36';
const H = { 'User-Agent': UA, Referer: 'https://vel.gg/bo1z/' };
const ROOT = process.argv[2] || 'pack-five';
const OUT = join(ROOT, 'files');
const PARTS = join(ROOT, '.parts');
const MANIFEST_URL = 'https://vel.gg/bo1z/manifest.json';
const PACKBASE = 'https://vel.gg/bo1z/pack/';
const CONCURRENCY = Number(process.env.CONC || 4);
const RETRIES = 4;
// Identity chunk size, from manifest.transport.chunk. Set after the manifest loads.
let CHUNK = 8388608;

const fmt = n => n > 1e6 ? (n / 1e6).toFixed(1) + ' MB' : (n / 1e3).toFixed(1) + ' KB';
const sha256 = b => createHash('sha256').update(b).digest('hex');

const log = (...a) => console.log(...a);
const seen = { files: 0, skipped: 0, bytes: 0, wire: 0, failed: [] };

async function fetchRetry(url, opts = {}, tries = RETRIES) {
  let lastErr;
  for (let i = 0; i < tries; i++) {
    try {
      const r = await fetch(url, { ...opts, headers: { ...H, ...(opts.headers || {}) } });
      if (r.status === 200 || r.status === 206) return r;
      lastErr = new Error('HTTP ' + r.status);
      if (r.status === 404) break; // pointless to retry
    } catch (e) { lastErr = e; }
    await new Promise(r => setTimeout(r, 400 * (i + 1)));
  }
  throw lastErr || new Error('fetch failed');
}

async function downloadFile(f) {
  const dest = join(OUT, f.path);
  // Already complete on a previous run?
  if (existsSync(dest)) {
    const buf = await readFile(dest);
    if (buf.length === f.size && sha256(buf) === f.sha256) {
      seen.skipped++; seen.bytes += buf.length;
      return `skip   ${f.path} (verified)`;
    }
  }
  await mkdir(dirname(dest), { recursive: true });

  // Verified protocol (probe-pack-protocol.mjs):
  //   chunked : GET pack/<path>?part=<k>   -> Content-Encoding: br, fetch DECODES it,
  //             body is exactly one identity chunk (8 MiB, last part short). Do NOT
  //             brotli-decompress it yourself.
  //   (pack/<path>.br/<k> serves the same part as RAW brotli, undecoded — that variant
  //    is what produced double-decompression failures. Not used here.)
  //   raw     : GET pack/<path> with Range: bytes=N-  -> 206 + Content-Range.
  const partCount = Math.ceil(f.size / CHUNK);
  const chunked = partCount > 1;
  const partDir = join(PARTS, f.path);
  await mkdir(partDir, { recursive: true });

  const chunks = [];
  for (let i = 0; i < partCount; i++) {
    const pf = join(partDir, String(i));
    const want = Math.min(CHUNK, f.size - i * CHUNK);
    let buf;
    if (existsSync(pf) && (await stat(pf)).size === want) {
      buf = await readFile(pf);
    } else {
      const url = PACKBASE + f.path + (chunked ? `?part=${i}` : '');
      const opts = chunked
        ? { cache: 'no-store' }
        : { cache: 'no-store', headers: i === 0 ? {} : { Range: `bytes=${i * CHUNK}-` } };
      const r = await fetchRetry(url, opts);
      buf = Buffer.from(await r.arrayBuffer());
      if (buf.length !== want) throw new Error(`part ${i} size ${buf.length} != ${want}`);
      await writeFile(pf + '.tmp', buf);
      await rename(pf + '.tmp', pf);
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
log(`fetching manifest: ${MANIFEST_URL}`);
const manifest = await (await fetch(MANIFEST_URL, { headers: H })).json();
CHUNK = (manifest.transport?.encoding === 'br' && manifest.transport.chunk > 0)
  ? manifest.transport.chunk : 8388608;

// Variant handling, straight from download-worker.js:
//   function useFallback(e){ const {size,sha256,br}=e.fallback; Object.assign(e,{size,sha256,br}); delete e.opus; }
// Entries carrying `opus` + `fallback` have TWO encodings of the same content:
//   * primary  : <path>?part=k is the fallback, while the REAL primary bytes live at
//                <path>.kop?part=k (Opus, requires an AudioDecoder to rebuild) or
//                <path>.dxs?part=k (DXT-stream-split), per the worker.
//   * fallback : plain identity chunks over fallback.size.
// The site picks the fallback whenever WebCodecs AudioDecoder is unavailable, so it is
// a fully supported variant. We prefer it: 181 MB smaller for sounds.pack, verified by
// the manifest's own fallback.sha256, and no Opus/ADPCM re-encode step needed.
// NOTE: chunking MUST follow the variant chosen, or parts are read at wrong offsets.
function normalize(f) {
  if (f.opus && f.fallback) {
    return { ...f, size: f.fallback.size, sha256: f.fallback.sha256,
             br: f.fallback.br, variant: 'fallback' };
  }
  return { ...f, variant: 'primary' };
}
const files = manifest.files.map(normalize);
const fallbackCount = files.filter(f => f.variant === 'fallback').length;
const totalWire = files.reduce((s, f) =>
  s + (Array.isArray(f.br) && f.br.length ? f.br.reduce((a, b) => a + b, 0) : f.size), 0);
const totalDisk = files.reduce((s, f) => s + f.size, 0);
log(`files=${files.length} (${fallbackCount} fallback variants) disk=${fmt(totalDisk)} wire=${fmt(totalWire)}`);
log(`output: ${OUT}   concurrency: ${CONCURRENCY}\n`);

await mkdir(OUT, { recursive: true });
const t0 = Date.now();

// Largest first: long poles start immediately, and progress feels real.
const queue = files.slice().sort((a, b) => b.size - a.size);
let done = 0;
const total = queue.length;

async function worker(id) {
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
await Promise.all(Array.from({ length: CONCURRENCY }, (_, i) => worker(i)));

const secs = (Date.now() - t0) / 1000;
log(`\n=== done in ${secs.toFixed(1)}s ===`);
log(`downloaded ${seen.files}, skipped ${seen.skipped}, failed ${seen.failed.length}`);
log(`unpacked bytes on disk: ${fmt(seen.bytes)}`);
log(`wire bytes pulled     : ${fmt(seen.wire)}`);
if (seen.failed.length) {
  log(`\nfailed files (re-run to retry, parts are cached):`);
  seen.failed.forEach(p => log('  ' + p));
  process.exitCode = 1;
} else {
  log('\nALL FILES VERIFIED against manifest sha256.');
}
