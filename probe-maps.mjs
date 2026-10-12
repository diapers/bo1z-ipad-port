// Price every map: fetch each slug's manifest and total up disk vs wire size.
// Slug layout is from the site's own maps.js MAP_LIST.
import { writeFileSync } from 'node:fs';
import { mkdir } from 'node:fs/promises';

const H = { 'User-Agent': 'Mozilla/5.0', Referer: 'https://vel.gg/bo1z/' };
const fmt = n => n >= 1e9 ? (n / 1e9).toFixed(2) + ' GB' : (n / 1e6).toFixed(1) + ' MB';

// zone name -> slug, straight from maps.js. 'five' is the exception: its manifest
// lives at the site root (/manifest.json), not under /five/.
const MAPS = [
  ['five', 'zombie_pentagon', 'Five'],
  ['kino', 'zombie_theater', 'Kino der Toten'],
  ['riese', 'zombie_cod5_factory', 'Der Riese'],
  ['nacht', 'zombie_cod5_prototype', 'Nacht der Untoten'],
  ['verruckt', 'zombie_cod5_asylum', 'Verrückt'],
  ['shinonuma', 'zombie_cod5_sumpf', 'Shi No Numa'],
  ['ascension', 'zombie_cosmodrome', 'Ascension'],
  ['cotd', 'zombie_coast', 'Call of the Dead'],
  ['shangrila', 'zombie_temple', 'Shangri-La'],
  ['moon', 'zombie_moon', 'Moon'],
];

const rows = [];
let totDisk = 0, totWire = 0;

for (const [slug, zone, name] of MAPS) {
  const urls = slug === 'five'
    ? ['https://vel.gg/bo1z/manifest.json']
    : [`https://vel.gg/bo1z/${slug}/manifest.json`, `https://vel.gg/bo1z/${slug}/pack/manifest.json`];
  let m = null, from = null;
  for (const u of urls) {
    try {
      const r = await fetch(u, { headers: H });
      if (!r.ok) continue;
      const j = await r.json();
      if (Array.isArray(j.files)) { m = j; from = u; break; }
    } catch {}
  }
  if (!m) { rows.push({ slug, zone, name, err: 'manifest not found' }); console.log(`${slug.padEnd(11)} MISSING`); continue; }

  // Variant-correct sizes: opus/fallback entries ship a smaller fallback.
  const disk = m.files.reduce((s, f) => s + ((f.opus && f.fallback) ? f.fallback.size : f.size), 0);
  const wire = m.files.reduce((s, f) => {
    const br = (f.opus && f.fallback) ? f.fallback.br : f.br;
    return s + (Array.isArray(br) && br.length ? br.reduce((a, b) => a + b, 0) : ((f.opus && f.fallback) ? f.fallback.size : f.size));
  }, 0);
  const streams = (m.streams || []).reduce((s, x) => s + x.size, 0);
  const fbCount = m.files.filter(f => f.opus && f.fallback).length;
  totDisk += disk + streams; totWire += wire + streams;

  rows.push({ slug, zone, name, files: m.files.length, disk, wire, streams, fbCount, version: m.version });
  console.log(`${slug.padEnd(11)} ${name.padEnd(20)} files=${String(m.files.length).padStart(3)}  disk=${fmt(disk).padStart(9)}  wire=${fmt(wire).padStart(9)}  streams=${fmt(streams).padStart(8)}  fb=${fbCount}`);
  // Save each manifest so the downloader/fetcher can be pointed at any map offline.
  await mkdir('manifests', { recursive: true });
  writeFileSync(`manifests/${slug}.json`, JSON.stringify(m, null, 1));
}

console.log(`\nTOTAL across ${MAPS.length} maps: disk=${fmt(totDisk)}  wire=${fmt(totWire)}`);
console.log('(five alone was 694.5 MB on disk / 232.5 MB wire)');
writeFileSync('manifests/_summary.json', JSON.stringify({ rows, totDisk, totWire }, null, 1));
console.log('manifests saved to manifests/<slug>.json');
