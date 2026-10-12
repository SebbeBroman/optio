/**
 * Minimal static file server for the browser smoke test. Serves this package
 * directory with `application/wasm` for .wasm (browsers refuse streaming
 * instantiation without it).
 *
 *   node serve.mjs [port]     # default 8123, then open the printed URL
 *
 * `startServer(port)` is also used by `test-http.mjs` to exercise the browser
 * loading path (URL + fetch) headlessly.
 */
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { extname, join, normalize, sep } from 'node:path';
import { fileURLToPath } from 'node:url';
import { dirname } from 'node:path';

const root = dirname(fileURLToPath(import.meta.url));

const mime = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.cjs': 'text/javascript; charset=utf-8',
  '.wasm': 'application/wasm',
  '.json': 'application/json; charset=utf-8',
};

/** @returns {Promise<import('node:http').Server>} the listening server */
export function startServer(port) {
  const server = createServer(async (req, res) => {
    try {
      const url = new URL(req.url, 'http://localhost');
      let path = normalize(decodeURIComponent(url.pathname)).replace(/^\/+/, '');
      if (path === '' || path.endsWith('/')) path += 'index.html';
      const file = join(root, path);
      if (!file.startsWith(root + sep) && file !== root) throw new Error('outside root');
      const body = await readFile(file);
      res.writeHead(200, { 'content-type': mime[extname(file)] ?? 'application/octet-stream' });
      res.end(body);
    } catch {
      res.writeHead(404, { 'content-type': 'text/plain' });
      res.end('not found');
    }
  });
  return new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(port, () => resolve(server));
  });
}

const isMain = process.argv[1] && import.meta.url.endsWith(process.argv[1].split('/').pop());
if (isMain) {
  const port = Number(process.argv[2] ?? 8123);
  await startServer(port);
  console.log(`serving ${root} at http://localhost:${port}/smoke.html`);
}
