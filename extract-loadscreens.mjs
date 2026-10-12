// Extract each map's own BO1 loading screen (images/loadscreen_<zone>.iwi inside the map's
// main/*.iwd) to <root>/maps/menu/loadscreen-<slug>.png, for the host's loading screen.
//
// Usage: node extract-loadscreens.mjs [--root=D:/bo1z]
//
// IWI v13 (T5): "IWi", version 0x0D, format, flags, u16 width, u16 height, u16 depth, then
// mip offsets; the image data ends the file (largest mip last). These are 1024x1024 DXT1
// (format 0x0B). The World at War maps use their original names (zombie_cod5_factory ->
// loadscreen_zombie_factory).
import { readdirSync, writeFileSync, mkdirSync, existsSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join, resolve } from 'node:path';
import { deflateSync } from 'node:zlib';

const args = Object.fromEntries(process.argv.slice(2).map(a => {
  const m = /^--([^=]+)(?:=(.*))?$/.exec(a);
  return m ? [m[1], m[2] ?? true] : [a, true];
}));
const ROOT = resolve(args.root || 'D:/bo1z');
const TAR = process.platform === 'win32' ? 'C:\\Windows\\System32\\tar.exe' : 'bsdtar';
const MAPS = {
  kino: 'zombie_theater', five: 'zombie_pentagon', riese: 'zombie_cod5_factory', nacht: 'zombie_cod5_prototype',
  verruckt: 'zombie_cod5_asylum', shinonuma: 'zombie_cod5_sumpf', ascension: 'zombie_cosmodrome',
  cotd: 'zombie_coast', shangrila: 'zombie_temple', moon: 'zombie_moon',
};

function readFromZip(zip, entry) {
  const r = spawnSync(TAR, ['-xOf', zip, entry], { maxBuffer: 64 << 20 });
  return r.status === 0 && r.stdout.length ? r.stdout : null;
}

function rgb565(c) {
  return [((c >> 11) & 31) * 255 / 31 | 0, ((c >> 5) & 63) * 255 / 63 | 0, (c & 31) * 255 / 31 | 0];
}

function decodeDxt1(data, w, h) {
  const out = Buffer.alloc(w * h * 4);
  let p = 0;
  for (let by = 0; by < h; by += 4) {
    for (let bx = 0; bx < w; bx += 4) {
      const c0 = data.readUInt16LE(p), c1 = data.readUInt16LE(p + 2), bits = data.readUInt32LE(p + 4);
      p += 8;
      const a = rgb565(c0), b = rgb565(c1);
      const pal = c0 > c1
        ? [a, b, a.map((v, i) => (2 * v + b[i]) / 3 | 0), a.map((v, i) => (v + 2 * b[i]) / 3 | 0)]
        : [a, b, a.map((v, i) => (v + b[i]) / 2 | 0), [0, 0, 0]];
      for (let i = 0; i < 16; i++) {
        const x = bx + (i & 3), y = by + (i >> 2);
        if (x >= w || y >= h) continue;
        const c = pal[(bits >>> (2 * i)) & 3], o = (y * w + x) * 4;
        out[o] = c[0]; out[o + 1] = c[1]; out[o + 2] = c[2]; out[o + 3] = 255;
      }
    }
  }
  return out;
}

const CRC = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) { let c = n; for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1; t[n] = c >>> 0; }
  return buf => { let c = 0xffffffff; for (const b of buf) c = t[(c ^ b) & 255] ^ (c >>> 8); return (c ^ 0xffffffff) >>> 0; };
})();

function png(rgba, w, h) {
  const chunk = (type, data) => {
    const len = Buffer.alloc(4); len.writeUInt32BE(data.length);
    const td = Buffer.concat([Buffer.from(type), data]);
    const crc = Buffer.alloc(4); crc.writeUInt32BE(CRC(td));
    return Buffer.concat([len, td, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 2; // RGB
  const raw = Buffer.alloc(h * (1 + w * 3));
  for (let y = 0; y < h; y++) {
    raw[y * (1 + w * 3)] = 0;
    for (let x = 0; x < w; x++) {
      const s = (y * w + x) * 4, d = y * (1 + w * 3) + 1 + x * 3;
      raw[d] = rgba[s]; raw[d + 1] = rgba[s + 1]; raw[d + 2] = rgba[s + 2];
    }
  }
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw, { level: 9 })), chunk('IEND', Buffer.alloc(0))]);
}

const outDir = join(ROOT, 'maps', 'menu');
mkdirSync(outDir, { recursive: true });
for (const [slug, zone] of Object.entries(MAPS)) {
  const dir = join(ROOT, 'maps', slug, 'main');
  if (!existsSync(dir)) { console.log(`${slug}: not mirrored`); continue; }
  const entry = `images/loadscreen_${zone.replace('cod5_', '')}.iwi`;
  let iwi = null;
  for (const f of readdirSync(dir).filter(n => n.endsWith('.iwd')).sort()) {
    iwi = readFromZip(join(dir, f), entry);
    if (iwi) break;
  }
  if (!iwi) { console.log(`${slug}: ${entry} not found`); continue; }
  if (iwi.toString('latin1', 0, 3) !== 'IWi' || iwi[3] !== 0x0d) { console.log(`${slug}: not an IWI v13`); continue; }
  const fmt = iwi[4], w = iwi.readUInt16LE(6), h = iwi.readUInt16LE(8);
  if (fmt !== 0x0b) { console.log(`${slug}: format ${fmt} (only DXT1 handled)`); continue; }
  const size = (Math.max(w, 4) / 4) * (Math.max(h, 4) / 4) * 8;
  const rgba = decodeDxt1(iwi.subarray(iwi.length - size), w, h);
  const out = join(outDir, `loadscreen-${slug}.png`);
  writeFileSync(out, png(rgba, w, h));
  console.log(`${slug}: ${entry} ${w}x${h} -> ${out}`);
}
