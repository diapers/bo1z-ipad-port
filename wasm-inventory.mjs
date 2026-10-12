import { readFileSync, writeFileSync } from 'node:fs';
const buf = readFileSync(process.argv[2]);
let p = 8;
const uleb = () => { let r=0,s=0,b; do { b=buf[p++]; r|=(b&0x7f)<<s; s+=7; } while (b&0x80); return r>>>0; };
const nm = () => { const n=uleb(); const s=buf.toString('utf8',p,p+n); p+=n; return s; };
const imports=[], exportsL=[];
while (p < buf.length) {
  const id = buf[p++]; const size = uleb(); const end = p+size;
  if (id===2){ const n=uleb(); for(let i=0;i<n;i++){ const mod=nm(), n2=nm(), k=buf[p++]; if(k===0){ imports.push({mod,name:n2,kind:'func',typeIdx:uleb()}); } else if(k===1){ const el=buf[p++]; const fl=uleb(); const mn=uleb(); if(fl&1) uleb(); imports.push({mod,name:n2,kind:'table',el,min:mn}); } else if(k===2){ const fl=uleb(); const mn=uleb(); let mx=0; if(fl&1) mx=uleb(); imports.push({mod,name:n2,kind:'memory',shared:!!(fl&2),min:mn,max:mx}); } else if(k===3){ buf[p++]; buf[p++]; imports.push({mod,name:n2,kind:'global'}); } else if(k===4){ uleb(); uleb(); imports.push({mod,name:n2,kind:'tag'}); } } }
  else if (id===7){ const n=uleb(); for(let i=0;i<n;i++){ const e=nm(), k=buf[p++]; const idx=uleb(); exportsL.push({name:e,kind:['func','table','memory','global','tag'][k]||k,idx}); } }
  p = end;
}
const funcs = imports.filter(i=>i.kind==='func');
const gl = funcs.filter(i=>/^gl/.test(i.name)||/^emscripten_webgl/.test(i.name)||/^emwgpu/.test(i.name)||/WebGPU/.test(i.name));
const other = funcs.filter(i=>!gl.includes(i));
const cat = n => {
  if (/^_emval|^_embind|emscripten_asm_const/.test(n)) return 'JS/embind bridge';
  if (/^__asyncjs__/.test(n)) return 'JSPI async bridge';
  if (/^wasi/.test(n)) return 'WASI';
  if (/console|_err$|_out$|abort|exit/.test(n)) return 'logging/exit';
  if (/socket|net|poll|select|sock/.test(n)) return 'networking';
  if (/time|date|now/.test(n)) return 'time';
  if (/audio|Audio/.test(n)) return 'audio';
  if (/thread|pthread|atomic|futex|Wait|Wake|mutex|sem/.test(n)) return 'threads';
  if (/^_wasmfs|filesystem|open|read|write|fstat/.test(n)) return 'filesystem';
  return 'other';
};
const groups={};
for (const f of other) (groups[cat(f.name)] ||= []).push(f.name);
let out = '# BO1Z wasm host-surface inventory\n\n';
out += `module: KisakBlack-web.wasm (${(buf.length/1e6).toFixed(2)} MB)\n`;
out += `imported funcs: ${funcs.length}   GL/GPU: ${gl.length}   other: ${other.length}\n`;
out += `exports: ${exportsL.length}   memory: ${JSON.stringify(imports.find(i=>i.kind==='memory'))}\n\n`;
out += `## Graphics / GPU imports (${gl.length}) - must be served by a native GL or wgpu layer\n`;
out += gl.map(i=>i.name).sort().join('\n') + '\n\n';
out += `## Non-graphics imports by category (${other.length})\n`;
for (const [k,v] of Object.entries(groups).sort((a,b)=>b[1].length-a[1].length)) {
  out += `\n### ${k} (${v.length})\n` + v.sort().join('\n') + '\n';
}
out += `\n## Exports (${exportsL.length})\n` + exportsL.map(e=>`${e.kind}\t${e.name}`).join('\n') + '\n';
writeFileSync(process.argv[3], out);
console.log(out.slice(0, 3000));
console.log('\n...\nwritten to ' + process.argv[3]);
console.log('GL/GPU import count: ' + gl.length + ' | non-GL: ' + other.length);
