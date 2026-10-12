// Serves bo1z-ipad-probe.html with the cross-origin isolation headers vel.gg itself sends.
// Needed because SharedArrayBuffer / Wasm threads are gated on COOP+COEP.
// Run: node bo1z-probe-server.mjs     then open the printed URL on the iPad.
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { networkInterfaces } from 'node:os';
import { fileURLToPath } from 'node:url';
import { dirname, join } from 'node:path';

const PORT = Number(process.env.PORT || 8137);
const DIR = dirname(fileURLToPath(import.meta.url));
const FILE = join(DIR, 'bo1z-ipad-probe.html');

const server = createServer(async (req, res) => {
  if (req.url === '/favicon.ico') { res.writeHead(204).end(); return; }
  try {
    const html = await readFile(FILE);
    res.writeHead(200, {
      'Content-Type': 'text/html; charset=utf-8',
      'Cross-Origin-Opener-Policy': 'same-origin',
      // credentialless (not require-corp): gives crossOriginIsolated + SharedArrayBuffer
      // without demanding CORP headers from every subresource we probe.
      'Cross-Origin-Embedder-Policy': 'credentialless',
      'Cache-Control': 'no-store',
    });
    res.end(html);
  } catch (e) {
    res.writeHead(500, { 'Content-Type': 'text/plain' }).end('read failed: ' + e.message);
  }
});

server.listen(PORT, '0.0.0.0', () => {
  const nets = networkInterfaces();
  const lan = Object.values(nets).flat()
    .filter(n => n && n.family === 'IPv4' && !n.internal)
    .map(n => n.address);
  console.log('probe server up on port ' + PORT);
  console.log('  local :  http://localhost:' + PORT + '/');
  for (const ip of lan) console.log('  iPad  :  http://' + ip + ':' + PORT + '/');
  console.log('\nOpen the iPad URL in Safari on the iPad (same Wi-Fi). If it will not load, allow');
  console.log('node.exe through Windows Defender Firewall for Private networks, or just run:');
  console.log('  New-NetFirewallRule -DisplayName "bo1z probe" -Direction Inbound -LocalPort ' + PORT + ' -Protocol TCP -Action Allow');
});
