// Build the full IPA: the asset-free BO1Z.ipa from CI + the engine + every map.
//
// Usage: node package-ipa.mjs --ipa=<ci BO1Z.ipa> [--root=D:/bo1z] [--maps=all|five,kino] [--out=<out.ipa>]
//                             [--wasm=bo1z-wasm/KisakBlack-native.wasm] [--verify]
//
// Maps share many files, so the bundle carries the content-addressed store once:
//   Payload/BO1Z.app/game/store/<ab>/<sha256>   every unique file a chosen map uses
//   Payload/BO1Z.app/game/maps/<slug>.json      {slug, files: [[path, sha256, size], ...]}
//   Payload/BO1Z.app/game/menu/mapselect-*.webp  the site's map-select art (<root>/maps/menu)
// The host (src/maps.rs + fs.rs index mounts) serves each map's paths from its index.
//
// Sources: <root>/maps/<slug>/manifest.json (site manifest) and <root>/store/<ab>/<sha256>
// (build-store.mjs). Variant rule as everywhere: opus+fallback entries use the fallback.
// Staging hard-links blobs (same volume), so it costs no extra disk; temp lives on <root>.
// --verify re-hashes every blob (slow: ~6 GB); default checks presence and size.
import { createHash } from 'node:crypto';
import { createReadStream, existsSync, linkSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync, copyFileSync } from 'node:fs';
import { rm } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';
import { join, resolve } from 'node:path';

const args = Object.fromEntries(process.argv.slice(2).map(a => {
  const m = /^--([^=]+)(?:=(.*))?$/.exec(a);
  return m ? [m[1], m[2] ?? true] : [a, true];
}));
if (!args.ipa) {
  console.error('usage: node package-ipa.mjs --ipa=<BO1Z.ipa> [--root=D:/bo1z] [--maps=all] [--out=..] [--verify]');
  process.exit(2);
}
const ROOT = resolve(args.root || 'D:/bo1z');
const ALL = ['five', 'kino', 'riese', 'nacht', 'verruckt', 'shinonuma', 'ascension', 'cotd', 'shangrila', 'moon'];
const MAPS = !args.maps || args.maps === 'all'
  ? ALL.filter(s => existsSync(join(ROOT, 'maps', s, 'manifest.json')))
  : String(args.maps).split(',').map(s => s.trim()).filter(Boolean);
const IPA = resolve(args.ipa);
const OUT = resolve(args.out || join(ROOT, `BO1Z-${MAPS.length === ALL.length ? 'all' : MAPS.join('-')}.ipa`));
const WASM = resolve(args.wasm || 'bo1z-wasm/KisakBlack-native.wasm');
const TAR = process.platform === 'win32' ? 'C:\\Windows\\System32\\tar.exe' : 'bsdtar';
const fmt = n => n >= 1e9 ? (n / 1e9).toFixed(2) + ' GB' : (n / 1e6).toFixed(1) + ' MB';
const blobPath = sha => join(ROOT, 'store', sha.slice(0, 2), sha);

const sha256File = p => new Promise((ok, fail) => {
  const h = createHash('sha256');
  createReadStream(p).on('data', d => h.update(d)).on('end', () => ok(h.digest('hex'))).on('error', fail);
});
function run(cmd, argv, cwd) {
  const r = spawnSync(cmd, argv, { cwd, stdio: 'inherit' });
  if (r.status !== 0) throw new Error(`${cmd} ${argv.join(' ')} exited ${r.status}`);
}

// ---- per-map indexes ---------------------------------------------------------------------
const blobs = new Map(); // sha -> size
const indexes = {};
for (const slug of MAPS) {
  const M = JSON.parse(readFileSync(join(ROOT, 'maps', slug, 'manifest.json'), 'utf8'));
  const files = [];
  for (const f of M.files) {
    const fb = f.opus && f.fallback;
    files.push([f.path, fb ? f.fallback.sha256 : f.sha256, fb ? f.fallback.size : f.size]);
  }
  for (const s of M.streams || []) files.push([s.path, s.sha256, s.size]);
  for (const [path, sha, size] of files) {
    const p = blobPath(sha);
    if (!existsSync(p)) throw new Error(`${slug}: ${path} -> missing blob ${sha} (run build-store.mjs)`);
    const actual = statSync(p).size;
    if (actual !== size) throw new Error(`${slug}: ${path} -> blob ${sha} is ${actual} bytes, manifest says ${size}`);
    blobs.set(sha, size);
  }
  indexes[slug] = { slug, files };
  const logical = files.reduce((a, f) => a + f[2], 0);
  console.log(`${slug.padEnd(10)} ${String(files.length).padStart(3)} files  ${fmt(logical)}`);
}
const unique = [...blobs.values()].reduce((a, b) => a + b, 0);
console.log(`unique blobs: ${blobs.size}, ${fmt(unique)}`);

if (args.verify) {
  let i = 0;
  for (const sha of blobs.keys()) {
    if (await sha256File(blobPath(sha)) !== sha) throw new Error(`blob ${sha} is corrupt`);
    if (++i % 50 === 0) process.stdout.write(`\rverified ${i}/${blobs.size}`);
  }
  console.log(`\rverified ${blobs.size}/${blobs.size} blobs`);
}

// ---- stage ---------------------------------------------------------------------------------
const tmp = join(ROOT, 'tmp-ipa');
await rm(tmp, { recursive: true, force: true });
mkdirSync(tmp, { recursive: true });
try {
  run(TAR, ['-xf', IPA], tmp);
  const payload = join(tmp, 'Payload');
  const apps = readdirSync(payload).filter(n => n.endsWith('.app'));
  if (apps.length !== 1) throw new Error(`expected one Payload/*.app, found ${apps.length}`);
  const app = join(payload, apps[0]);

  if (!existsSync(WASM)) throw new Error(`engine not found: ${WASM}`);
  copyFileSync(WASM, join(app, 'KisakBlack-native.wasm'));

  const game = join(app, 'game');
  mkdirSync(join(game, 'maps'), { recursive: true });
  // The site's map-select art (mapselect-*.webp) for the launch menu, if mirrored.
  const menuSrc = join(ROOT, 'maps', 'menu');
  if (existsSync(menuSrc)) {
    mkdirSync(join(game, 'menu'), { recursive: true });
    const art = readdirSync(menuSrc).filter(n => n.endsWith('.webp'));
    for (const n of art) copyFileSync(join(menuSrc, n), join(game, 'menu', n));
    console.log(`menu art: ${art.length} images`);
  } else console.log('menu art: none (plain selector)');
  for (const [slug, idx] of Object.entries(indexes)) writeFileSync(join(game, 'maps', `${slug}.json`), JSON.stringify(idx));
  for (const sha of blobs.keys()) {
    const dir = join(game, 'store', sha.slice(0, 2));
    mkdirSync(dir, { recursive: true });
    linkSync(blobPath(sha), join(dir, sha)); // same volume: no copy
  }

  console.log(`zipping ${fmt(unique)} into ${OUT}`);
  await rm(OUT, { force: true });
  run(TAR, ['--format', 'zip', '--options', 'zip:compression=store', '-cf', OUT, 'Payload'], tmp);
  console.log(`done: ${OUT} (${fmt(statSync(OUT).size)})`);
} finally {
  await rm(tmp, { recursive: true, force: true });
}
