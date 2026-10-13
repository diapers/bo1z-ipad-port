// Per-map program manifests for the engine's own warm-up (KB_SetProgramManifest), in the
// site's format: {"format":"kisak-program-manifest-1","translator":T,"programs":[[vs,ps,mask],...]}
// (vs/ps = shader hashes in the map's web/shaders/shaders.pack, mask = vertex inputs the engine
// rewrites to BGRA: bit N for vs_vN).
//
// Sources: the host's recordings (<root>/maps/shadercache/<slug>.json: the exact GLSL the
// engine linked) mapped back to pack hashes, plus the site's manifest when present
// (--site=<dir with <slug>-programs.json>; only five has one).
//
// Usage: node build-program-manifests.mjs [--root=D:/bo1z] [--site=<dir>] [--maps=five,kino]
// Writes <root>/maps/shadercache/<slug>-programs.json (game data: never commit).

import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { join } from 'node:path';
import { inflateSync } from 'node:zlib';

const args = Object.fromEntries(process.argv.slice(2).map(a => {
  const m = /^--([^=]+)(?:=(.*))?$/.exec(a);
  return m ? [m[1], m[2] ?? true] : [a, true];
}));
const ROOT = args.root ?? 'D:/bo1z';
const SITE = args.site ?? null;
const MAPS = (args.maps ?? 'kino,five,riese,nacht,verruckt,shinonuma,ascension,cotd,shangrila,moon').split(',');

// KSPK: "KSPK" u32 version u32 count u32 indexEnd, then entries from 0x18:
// u32 offset, u32 size, u32 type, u32 nameLength, 32-byte digest, name. Type 2 = shader:
// u32 raw size + zlib JSON {source, stage, translator_key, ...}.
function readPack(path) {
  const d = readFileSync(path);
  if (d.toString('latin1', 0, 4) !== 'KSPK') throw new Error(`${path}: not a KSPK pack`);
  const indexEnd = d.readUInt32LE(12);
  const bySource = new Map();
  let translator = null;
  for (let p = 0x18; p < indexEnd;) {
    const off = d.readUInt32LE(p), size = d.readUInt32LE(p + 4), type = d.readUInt32LE(p + 8), nl = d.readUInt32LE(p + 12);
    const name = d.toString('latin1', p + 48, p + 48 + nl);
    p += 48 + nl;
    if (type !== 2) continue;
    const j = JSON.parse(inflateSync(d.subarray(off + 4, off + size)).toString('utf8'));
    bySource.set(j.source, name);
    translator ??= j.translator_key;
  }
  return { bySource, translator };
}

// The host adds `invariant gl_Position;`; the engine rewrites D3DCOLOR inputs.
function packVertexSource(src) {
  let mask = 0;
  let s = src.replace('#version 300 es\ninvariant gl_Position;', '#version 300 es');
  s = s.replace(/in vec4 vs_v(\d+)_bgra;\n#define vs_v\1 \(vs_v\1_bgra\.bgra\)\n\n/g, (_, n) => {
    mask |= 1 << Number(n);
    return `in vec4 vs_v${n};\n`;
  });
  return { s, mask };
}

for (const slug of MAPS) {
  const packPath = join(ROOT, 'maps', slug, 'web', 'shaders', 'shaders.pack');
  if (!existsSync(packPath)) { console.log(`${slug}: no shader pack`); continue; }
  const { bySource, translator } = readPack(packPath);
  const out = [];
  const seen = new Set();
  const add = (vs, ps, mask) => {
    const key = `${vs}/${ps}/${mask}`;
    if (!seen.has(key)) { seen.add(key); out.push([vs, ps, String(mask)]); }
  };
  let site = 0;
  if (SITE && existsSync(join(SITE, `${slug}-programs.json`))) {
    try {
      const m = JSON.parse(readFileSync(join(SITE, `${slug}-programs.json`), 'utf8'));
      for (const [vs, ps, mask] of m.programs ?? []) { add(vs, ps, mask); site++; }
    } catch { /* "Not found" pages */ }
  }
  let recorded = 0, unmatched = 0;
  const recPath = join(ROOT, 'maps', 'shadercache', `${slug}.json`);
  if (existsSync(recPath)) {
    const r = JSON.parse(readFileSync(recPath, 'utf8'));
    for (const [vh, fh] of r.programs) {
      const v = packVertexSource(r.shaders[vh] ?? '');
      const vs = bySource.get(v.s), ps = bySource.get(r.shaders[fh] ?? '');
      if (!vs || !ps) { unmatched++; continue; }
      add(vs, ps, v.mask);
      recorded++;
    }
  }
  const manifest = { format: 'kisak-program-manifest-1', translator, programs: out };
  writeFileSync(join(ROOT, 'maps', 'shadercache', `${slug}-programs.json`), JSON.stringify(manifest));
  console.log(`${slug.padEnd(10)} ${String(out.length).padStart(4)} programs (site ${site}, recorded ${recorded}, unmatched ${unmatched})`);
}
