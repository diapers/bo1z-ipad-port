// Print every function import/export of a wasm module with its signature.
// Usage: node tools/wasm-sigs.mjs <file.wasm> [imports|exports]
import { readFileSync } from 'node:fs';
const buf = readFileSync(process.argv[2]);
const want = process.argv[3] || 'imports';
let p = 8;
const uleb = () => { let r = 0, s = 0, b; do { b = buf[p++]; r += (b & 0x7f) * 2 ** s; s += 7; } while (b & 0x80); return r; };
const name = () => { const n = uleb(); const s = buf.toString('utf8', p, p + n); p += n; return s; };
const VT = { 0x7f: 'i32', 0x7e: 'i64', 0x7d: 'f32', 0x7c: 'f64', 0x7b: 'v128', 0x70: 'funcref', 0x6f: 'externref' };
const types = [], funcs = [], out = [];
let importedFuncs = 0;
while (p < buf.length) {
  const id = buf[p++], size = uleb(), end = p + size;
  if (id === 1) {
    for (let n = uleb(), i = 0; i < n; i++) {
      const form = buf[p++];
      if (form !== 0x60) throw new Error('non-func type ' + form);
      const params = [], results = [];
      for (let k = uleb(); k--;) params.push(VT[buf[p++]]);
      for (let k = uleb(); k--;) results.push(VT[buf[p++]]);
      types.push({ params, results });
    }
  } else if (id === 2) {
    for (let n = uleb(), i = 0; i < n; i++) {
      const mod = name(), nm = name(), kind = buf[p++];
      if (kind === 0) { const t = types[uleb()]; funcs.push(t); importedFuncs++; if (want === 'imports') out.push(`${mod}\t${nm}\t(${t.params.join(',')})->(${t.results.join(',')})`); }
      else if (kind === 1) { p++; const f = uleb(); uleb(); if (f & 1) uleb(); }
      else if (kind === 2) { const f = uleb(); uleb(); if (f & 1) uleb(); }
      else if (kind === 3) { p += 2; }
      else if (kind === 4) { p++; uleb(); }
    }
  } else if (id === 3) {
    for (let n = uleb(), i = 0; i < n; i++) funcs.push(types[uleb()]);
  } else if (id === 7 && want === 'exports') {
    for (let n = uleb(), i = 0; i < n; i++) {
      const nm = name(), kind = buf[p++], idx = uleb();
      if (kind === 0) { const t = funcs[idx]; out.push(`${nm}\t(${t.params.join(',')})->(${t.results.join(',')})`); }
      else out.push(`${nm}\tkind=${kind}`);
    }
  }
  p = end;
}
console.log(out.join('\n'));
