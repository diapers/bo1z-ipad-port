// Convert each map's loading cinematic (web/video/<zone>_load.webm, VP9 + Opus, which iOS
// cannot play) to H.264 + AAC MP4 at <root>/maps/menu/load-<slug>.mp4 for the iOS app.
// package-ipa.mjs copies them into game/menu/.
//
// Usage: node convert-videos.mjs [--root=D:/bo1z] [--force]
// Needs ffmpeg on PATH. Already-converted videos are skipped unless --force.
import { existsSync, readFileSync, renameSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join, resolve } from 'node:path';

const args = Object.fromEntries(process.argv.slice(2).map(a => {
  const m = /^--([^=]+)(?:=(.*))?$/.exec(a);
  return m ? [m[1], m[2] ?? true] : [a, true];
}));
const ROOT = resolve(args.root || 'D:/bo1z');
const SLUGS = ['kino', 'five', 'riese', 'nacht', 'verruckt', 'shinonuma', 'ascension', 'cotd', 'shangrila', 'moon'];

for (const slug of SLUGS) {
  const manifest = join(ROOT, 'maps', slug, 'manifest.json');
  if (!existsSync(manifest)) { console.log(`${slug}: not mirrored`); continue; }
  const M = JSON.parse(readFileSync(manifest, 'utf8'));
  // The VP9 stream (not the .av1.webm variant).
  const v = (M.streams || []).find(s => s.kind === 'video' && /_load\.webm$/.test(s.path) && !/\.av1\./.test(s.path));
  if (!v) { console.log(`${slug}: no loading video in manifest`); continue; }
  const src = join(ROOT, 'maps', slug, v.path);
  const out = join(ROOT, 'maps', 'menu', `load-${slug}.mp4`);
  if (existsSync(out) && !args.force) { console.log(`${slug}: already converted`); continue; }
  const tmp = out + '.part.mp4';
  const r = spawnSync('ffmpeg', ['-loglevel', 'error', '-y', '-i', src,
    '-c:v', 'libx264', '-profile:v', 'high', '-pix_fmt', 'yuv420p', '-crf', '21', '-preset', 'medium',
    '-c:a', 'aac', '-b:a', '160k', '-movflags', '+faststart', tmp], { stdio: 'inherit' });
  if (r.status !== 0) { console.log(`${slug}: ffmpeg failed (${r.status})`); continue; }
  renameSync(tmp, out);
  console.log(`${slug}: ${v.path} -> ${out}`);
}
