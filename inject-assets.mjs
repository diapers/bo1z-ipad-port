// Add locally mirrored map packs to an asset-free IPA built by CI.
//
// Usage: node inject-assets.mjs --ipa=<in.ipa> [--maps=five,kino] [--out=<out.ipa>] [--wasm=<engine.wasm>]
//
// Also copies the engine (default bo1z-wasm/KisakBlack-native.wasm: the site's wasm with legacy
// exceptions translated by wasm-opt --translate-to-exnref, see PROGRESS.md) into the app root.
//
// Layout inside the app bundle:
//   Payload/<App>.app/game/<slug>/manifest.json
//   Payload/<App>.app/game/<slug>/<pack paths...>
//
// Every file is hash-checked against manifests/<slug>.json (offline, variant-correct)
// before it goes in. An IPA is a plain zip; TrollStore re-signs on install, so adding
// resources after the CI build does not need a re-sign here.
//
// Zipping uses Windows' bsdtar (System32\tar.exe), which writes zip64 when needed.
// Entries are stored, not deflated: the packs are already compressed.
import { createHash } from 'node:crypto';
import { createReadStream, existsSync, readdirSync, readFileSync, statSync } from 'node:fs';
import { cp, mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { basename, join, resolve } from 'node:path';
import { tmpdir } from 'node:os';

const args = Object.fromEntries(process.argv.slice(2).map(a => {
  const m = /^--([^=]+)(?:=(.*))?$/.exec(a);
  return m ? [m[1], m[2] ?? true] : [a, true];
}));
if (!args.ipa) {
  console.error('usage: node inject-assets.mjs --ipa=<in.ipa> [--maps=five,kino] [--out=<out.ipa>]');
  process.exit(2);
}
const IPA = resolve(args.ipa);
const MAPS = String(args.maps || 'five,kino').split(',').map(s => s.trim()).filter(Boolean);
const OUT = resolve(args.out || IPA.replace(/\.ipa$/i, '') + '-with-assets.ipa');
const WASM = resolve(args.wasm || 'bo1z-wasm/KisakBlack-native.wasm');
const TAR = process.platform === 'win32' ? 'C:\\Windows\\System32\\tar.exe' : 'bsdtar';
const fmt = n => n >= 1e9 ? (n / 1e9).toFixed(2) + ' GB' : (n / 1e6).toFixed(1) + ' MB';

const sha256File = p => new Promise((ok, fail) => {
  const h = createHash('sha256');
  createReadStream(p).on('data', d => h.update(d)).on('end', () => ok(h.digest('hex'))).on('error', fail);
});

function run(cmd, argv, cwd) {
  const r = spawnSync(cmd, argv, { cwd, stdio: 'inherit' });
  if (r.status !== 0) throw new Error(`${cmd} ${argv.join(' ')} exited ${r.status}`);
}

// Where this map's file tree lives. The merged content-addressed store is now the
// primary source (all 10 maps, deduped to 6.61 GB via hard links); the old
// pack-<slug>/files layout is still accepted if it exists.
const STORE = resolve(args.store || 'E:/bo1z/maps');
function mapRoot(slug) {
  const candidates = [join(STORE, slug), join(`pack-${slug}`, 'files')];
  for (const c of candidates) if (existsSync(join(c, 'manifest.json')) || existsSync(c)) return c;
  return candidates[0];
}

// Same variant rule as verify-map.mjs: opus+fallback entries are mirrored as the fallback.
async function verifyMap(slug) {
  const M = JSON.parse(readFileSync(join('manifests', `${slug}.json`), 'utf8'));
  const root = mapRoot(slug);
  if (!existsSync(root)) throw new Error(`no asset tree for "${slug}" (looked in ${root}); pass --store=<dir>`);
  const entries = [
    ...M.files.map(f => {
      const fb = f.opus && f.fallback;
      return { path: f.path, size: fb ? f.fallback.size : f.size, sha256: fb ? f.fallback.sha256 : f.sha256 };
    }),
    ...(M.streams || []).map(s => ({ path: s.path, size: s.size, sha256: s.sha256 })),
  ];
  let bytes = 0;
  const bad = [];
  for (const e of entries) {
    const p = join(root, e.path);
    if (!existsSync(p)) { bad.push(['MISSING', e.path]); continue; }
    const size = statSync(p).size;
    if (size !== e.size || await sha256File(p) !== e.sha256) { bad.push(['BAD', e.path]); continue; }
    bytes += size;
  }
  return { M, root, count: entries.length, bytes, bad };
}

const tmp = await mkdtemp(join(tmpdir(), 'bo1z-ipa-'));
try {
  console.log(`unpacking ${basename(IPA)}`);
  run(TAR, ['-xf', IPA], tmp);
  const payload = join(tmp, 'Payload');
  const apps = existsSync(payload) ? readdirSync(payload).filter(n => n.endsWith('.app')) : [];
  if (apps.length !== 1) throw new Error(`expected one Payload/*.app, found ${apps.length}`);
  const app = join(payload, apps[0]);

  if (!existsSync(WASM)) throw new Error(`engine not found: ${WASM}`);
  await cp(WASM, join(app, 'KisakBlack-native.wasm'));
  console.log(`engine: ${basename(WASM)} (${fmt(statSync(WASM).size)})`);

  let total = 0;
  for (const slug of MAPS) {
    process.stdout.write(`verifying ${slug} ... `);
    const v = await verifyMap(slug);
    if (v.bad.length) {
      console.log('FAILED');
      v.bad.forEach(([k, p]) => console.log(`  ${k}  ${p}`));
      throw new Error(`${slug}: pack does not match manifests/${slug}.json; re-run fetch-map.mjs --map=${slug}`);
    }
    console.log(`${v.count} files ok, ${fmt(v.bytes)}`);
    const dest = join(app, 'game', slug);
    await mkdir(dest, { recursive: true });
    await cp(v.root, dest, { recursive: true });
    await writeFile(join(dest, 'manifest.json'), JSON.stringify(v.M));
    total += v.bytes;
  }

  console.log(`zipping ${fmt(total)} of assets into ${basename(OUT)}`);
  await rm(OUT, { force: true });
  run(TAR, ['--format', 'zip', '--options', 'zip:compression=store', '-cf', OUT, 'Payload'], tmp);
  console.log(`done: ${OUT} (${fmt(statSync(OUT).size)})`);
} finally {
  await rm(tmp, { recursive: true, force: true });
}
