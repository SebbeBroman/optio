#!/usr/bin/env node
/**
 * Build the publishable `optio` npm package.
 *
 * Reuses the workspace's canonical wasm build (`crates/wasm/build.sh`) for
 * cargo + wasm-bindgen + `wasm-opt`, then assembles `npm/wasm/`:
 *
 *   wasm/optio_wasm.js        web-target ESM glue (browsers, bundlers, Node ESM)
 *   wasm/optio_wasm.cjs       nodejs-target CJS glue (require(); sync auto-init)
 *   wasm/optio_wasm_bg.wasm   the binary, shared by both glues
 *
 * Both glue targets are generated from the same .wasm, so the package ships a
 * single binary; the build fails loudly if the two builds ever disagree.
 *
 * Env (forwarded to build.sh):
 *   OPTIO_STABLE=1     force the stable Rust path (skip nightly build-std)
 *   OPTIO_SMALL_ALLOC=1 build with the optional smaller free-list allocator
 *   OPTIO_NO_WASM_OPT=1 skip the wasm-opt size pass
 */
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const npmDir = dirname(fileURLToPath(import.meta.url));
const root = resolve(npmDir, '..');
const require = createRequire(import.meta.url);

function run(cmd, args, env) {
  const result = spawnSync(cmd, args, {
    cwd: root,
    stdio: 'inherit',
    env: { ...process.env, ...env },
    shell: process.platform === 'win32',
  });
  if (result.status !== 0) {
    throw new Error(`${cmd} ${args.join(' ')} failed with status ${result.status}`);
  }
}

function sha256(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex');
}

// ---------------------------------------------------------------------------
// 1. Build the wasm + glue for both targets via the workspace build script.
// ---------------------------------------------------------------------------

const stage = join(root, 'target', 'npm-build');
const webDir = join(stage, 'web');
const nodeDir = join(stage, 'node');
rmSync(stage, { recursive: true, force: true });
mkdirSync(webDir, { recursive: true });
mkdirSync(nodeDir, { recursive: true });

console.log('npm build: web target (ESM glue)');
run('bash', ['crates/wasm/build.sh'], {
  WASM_BINDGEN_TARGET: 'web',
  PKG_DIR: webDir,
});

console.log('npm build: nodejs target (CJS glue)');
run('bash', ['crates/wasm/build.sh'], {
  WASM_BINDGEN_TARGET: 'nodejs',
  PKG_DIR: nodeDir,
});

// ---------------------------------------------------------------------------
// 2. Assemble npm/wasm/ — one binary, two glues.
// ---------------------------------------------------------------------------

const webWasm = join(webDir, 'optio_wasm_bg.wasm');
const nodeWasm = join(nodeDir, 'optio_wasm_bg.wasm');
const webHash = sha256(webWasm);
const nodeHash = sha256(nodeWasm);
if (webHash !== nodeHash) {
  throw new Error(
    `wasm binary differs between glue targets (${webHash.slice(0, 12)} vs ${nodeHash.slice(0, 12)}); ` +
      'the single-binary layout assumes identical builds',
  );
}

const outDir = join(npmDir, 'wasm');
rmSync(outDir, { recursive: true, force: true });
mkdirSync(outDir, { recursive: true });

// Strip the `@ts-self-types` header that points at generated d.ts files we do
// not ship; the public types live in index.d.ts.
const stripSelfTypes = (source) => source.replace(/^\/\* @ts-self-types=[^\n]*\*\/\n/, '');

copyFileSync(webWasm, join(outDir, 'optio_wasm_bg.wasm'));
writeFileSync(
  join(outDir, 'optio_wasm.js'),
  stripSelfTypes(readFileSync(join(webDir, 'optio_wasm.js'), 'utf8')),
);
writeFileSync(
  join(outDir, 'optio_wasm.cjs'),
  stripSelfTypes(readFileSync(join(nodeDir, 'optio_wasm.js'), 'utf8')),
);

// Sanity: each glue must reference the binary in its own directory.
const esmGlue = readFileSync(join(outDir, 'optio_wasm.js'), 'utf8');
const cjsGlue = readFileSync(join(outDir, 'optio_wasm.cjs'), 'utf8');
if (!esmGlue.includes("new URL('optio_wasm_bg.wasm', import.meta.url)")) {
  throw new Error('unexpected ESM glue layout: default wasm URL not found');
}
if (!cjsGlue.includes('__dirname')) {
  throw new Error('unexpected CJS glue layout: __dirname-based wasm path not found');
}

// ---------------------------------------------------------------------------
// 3. Keep the package version in sync with the workspace version.
// ---------------------------------------------------------------------------

const cargo = readFileSync(join(root, 'Cargo.toml'), 'utf8');
const workspace = cargo.slice(cargo.indexOf('[workspace.package]'));
const cargoVersion = /version\s*=\s*"([^"]+)"/.exec(workspace)?.[1];
if (!cargoVersion) throw new Error('could not read [workspace.package] version from Cargo.toml');

const pkgPath = join(npmDir, 'package.json');
const pkg = JSON.parse(readFileSync(pkgPath, 'utf8'));
if (pkg.version !== cargoVersion) {
  console.log(`npm build: version ${pkg.version} -> ${cargoVersion} (workspace)`);
  pkg.version = cargoVersion;
  writeFileSync(pkgPath, JSON.stringify(pkg, null, 2) + '\n');
}

// ---------------------------------------------------------------------------
// 4. Report.
// ---------------------------------------------------------------------------

const kb = (path) => (statSync(path).size / 1024).toFixed(1);
console.log(`built npm package (${pkg.version}):`);
console.log(`  wasm/optio_wasm_bg.wasm  ${kb(join(outDir, 'optio_wasm_bg.wasm'))} KB`);
console.log(`  wasm/optio_wasm.js       ${kb(join(outDir, 'optio_wasm.js'))} KB (ESM)`);
console.log(`  wasm/optio_wasm.cjs      ${kb(join(outDir, 'optio_wasm.cjs'))} KB (CJS)`);
console.log(`  stage kept for inspection: ${stage}`);
console.log('next: npm pack --dry-run (in npm/) to inspect the tarball');
