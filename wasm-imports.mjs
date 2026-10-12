// Dump the import surface of a WebAssembly module.
// The import list IS the porting worklist: every import is a host function that
// a native (wasmtime/wgpu) host must reimplement, and every export is a reverse bridge.
import { readFileSync } from 'node:fs';

const file = process.argv[2];
const buf = readFileSync(file);
let p = 8; // skip \0asm + version

function uleb() {
  let r = 0, s = 0, b;
  do { b = buf[p++]; r |= (b & 0x7f) << s; s += 7; } while (b & 0x80);
  return r >>> 0;
}
function sleb() {
  let r = 0, s = 0, b;
  do { b = buf[p++]; r |= (b & 0x7f) << s; s += 7; } while (b & 0x80);
  if (s < 32 && (b & 0x40)) r |= -(1 << s);
  return r;
}
function name() {
  const n = uleb();
  const s = buf.toString('utf8', p, p + n);
  p += n;
  return s;
}

const TYPES = { 0x60: 'func', 0x5f: 'struct', 0x5e: 'array', 0x50: 'sub' };
const VAL = { 0x7f: 'i32', 0x7e: 'i64', 0x7d: 'f32', 0x7c: 'f64', 0x7b: 'v128',
              0x70: 'funcref', 0x6f: 'externref', 0x64: 'i64?', 0x63: 'f32?' };
const KIND = { 0: 'func', 1: 'table', 2: 'memory', 3: 'global', 4: 'tag' };

const funcTypes = [];   // typeidx per defined func
const imports = [];
const sections = [];
let exportCount = 0, funcCount = 0, memoryCount = 0, tableCount = 0, tagCount = 0, globalCount = 0;
let hasThreads = false, hasSIMD = false;
const featureHints = { sharedMemory: false, maxPages: 0, initialPages: 0 };

function readLimits() {
  const flags = uleb();
  const min = uleb();
  let max = 0;
  if (flags & 1) max = uleb();
  return { flags, min, max, shared: !!(flags & 2), is64: !!(flags & 4) };
}

while (p < buf.length) {
  const id = buf[p++];
  const size = uleb();
  const end = p + size;
  switch (id) {
    case 0: { // custom
      const nm = name();
      sections.push({ id, name: nm, size });
      break;
    }
    case 1: { // type
      const n = uleb();
      for (let i = 0; i < n; i++) {
        const form = buf[p++];
        if (form === 0x60) {
          const np = uleb();
          for (let k = 0; k < np; k++) { const v = buf[p++]; if (v === 0x7b) hasSIMD = true; p += (VAL[v] ? 0 : 0); }
          const nr = uleb();
          for (let k = 0; k < nr; k++) { const v = buf[p++]; if (v === 0x7b) hasSIMD = true; }
        } else { p = end; }
      }
      sections.push({ id, name: 'type', size, count: n });
      break;
    }
    case 2: { // import
      const n = uleb();
      for (let i = 0; i < n; i++) {
        const mod = name(), nm = name(), k = buf[p++];
        if (k === 0) { imports.push({ mod, name: nm, kind: 'func', typeIdx: uleb() }); funcCount++; }
        else if (k === 1) { const el = buf[p++]; readLimits(); if (el === 0x70 || el === 0x6f) {} imports.push({ mod, name: nm, kind: 'table' }); tableCount++; }
        else if (k === 2) { const lim = readLimits(); if (lim.shared) { hasThreads = true; featureHints.sharedMemory = true; } featureHints.initialPages = lim.min; featureHints.maxPages = lim.max; imports.push({ mod, name: nm, kind: 'memory', ...lim }); memoryCount++; }
        else if (k === 3) { buf[p++]; buf[p++]; imports.push({ mod, name: nm, kind: 'global' }); globalCount++; }
        else if (k === 4) { const a = uleb(); uleb(); imports.push({ mod, name: nm, kind: 'tag', attr: a }); tagCount++; }
      }
      sections.push({ id, name: 'import', size, count: n });
      break;
    }
    case 3: { // function
      const n = uleb();
      for (let i = 0; i < n; i++) funcTypes.push(uleb());
      funcCount += n;
      sections.push({ id, name: 'function', size, count: n });
      break;
    }
    case 5: { const lim = readLimits(); if (lim.shared) { hasThreads = true; featureHints.sharedMemory = true; } featureHints.initialPages = lim.min; featureHints.maxPages = lim.max; memoryCount++; sections.push({ id, name: 'memory', size, ...lim }); break; }
    case 7: { // export
      const n = uleb();
      for (let i = 0; i < n; i++) { name(); buf[p++]; uleb(); exportCount++; }
      sections.push({ id, name: 'export', size, count: n });
      break;
    }
    case 12: { const n = uleb(); sections.push({ id, name: 'data_count', size, count: n }); break; }
    case 13: { const n = uleb(); tagCount += n; hasThreads = true; sections.push({ id, name: 'tag', size, count: n }); break; }
    default: sections.push({ id, name: 'section' + id, size });
  }
  p = end;
}

// ---- report ----
console.log('file            : ' + file);
console.log('size            : ' + (buf.length / 1e6).toFixed(2) + ' MB');
console.log('defined funcs   : ' + funcTypes.length);
console.log('imports (func)  : ' + imports.filter(i => i.kind === 'func').length);
console.log('exports         : ' + exportCount);
console.log('memories        : ' + memoryCount);
console.log('tables          : ' + tableCount);
console.log('globals (imp)   : ' + globalCount);
console.log('tags (exn)      : ' + tagCount);
console.log('threads/shared  : ' + hasThreads);
console.log('SIMD (v128)     : ' + hasSIMD);
console.log('memory initial  : ' + featureHints.initialPages + ' pages (' + (featureHints.initialPages * 65536 / 1048576).toFixed(1) + ' MB)');
console.log('memory max      : ' + featureHints.maxPages + ' pages (' + (featureHints.maxPages * 65536 / 1048576).toFixed(0) + ' MB)');
console.log('');
console.log('sections: ' + sections.map(s => s.name + (s.count ? '[' + s.count + ']' : '')).join(', '));
console.log('');
console.log('=== IMPORT MODULES (host surface to reimplement) ===');
const byMod = {};
for (const im of imports) (byMod[im.mod] ||= []).push(im);
const ranked = Object.entries(byMod).sort((a, b) => b[1].length - a[1].length);
for (const [mod, list] of ranked) {
  console.log(`  ${mod.padEnd(24)} ${String(list.length).padStart(5)}  ${[...new Set(list.map(i => i.kind))].join('/')}`);
}
console.log('');
console.log('=== TOTAL IMPORT COUNT BY MODULE (ranked) ===');
console.log('  ' + ranked.map(([m, l]) => m + '=' + l.length).join('  '));
console.log('');
console.log('=== FILESYSTEM / IO IMPORT NAMES (sample of what a native host must serve) ===');
const fsImports = imports.filter(i => /^(fd_|path_|filesystem|__syscall_)/.test(i.name) || /fd_|path_/.test(i.name));
console.log('  count: ' + fsImports.length);
console.log('  ' + fsImports.slice(0, 45).map(i => i.name).join(', '));
console.log('');
console.log('=== JSPI / SUSPENDING BRIDGE IMPORTS ===');
const jspi = imports.filter(i => /suspend|promis|jspi|async/i.test(i.name) || /env|wasi/.test(i.mod) && /suspend/i.test(i.name));
console.log(jspi.length ? jspi.map(i => i.mod + '.' + i.name).join('\n  ') : '  (none found by name)');
console.log('');
console.log('=== ENV IMPORTS (the Emscripten runtime bridge) — first 60 ===');
const env = imports.filter(i => i.mod === 'env');
console.log('  count: ' + env.length);
console.log('  ' + env.slice(0, 60).map(i => i.name).join(', '));
console.log('');
console.log('=== wasi_snapshot_preview1 COUNT ===');
console.log('  ' + imports.filter(i => i.mod.startsWith('wasi')).length);
