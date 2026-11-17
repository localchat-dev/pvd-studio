import { writeFileSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

const modules = resolve(process.argv[2]);
for (const name of ['core', 'generators', 'wasm']) {
  const pkg = JSON.parse(readFileSync(resolve(modules, `@recast-navigation/${name}/package.json`)));
  if (pkg.version !== '0.43.1') throw new Error(`Expected ${name} 0.43.1, found ${pkg.version}`);
}
const core = await import(pathToFileURL(resolve(modules, '@recast-navigation/core/dist/index.mjs')));
const generators = await import(pathToFileURL(resolve(modules, '@recast-navigation/generators/dist/index.mjs')));
await core.init();
const result = generators.generateSoloNavMesh(
  [-5, 0, -5, -5, 0, 5, 5, 0, 5, 5, 0, -5], [0, 1, 2, 0, 2, 3],
  { cs: 0.5, ch: 0.2, walkableRadius: 0, minRegionArea: 0, mergeRegionArea: 0 },
);
if (!result.success) throw new Error(result.error);
const bytes = core.exportNavMesh(result.navMesh);
// The upstream importer supplies expected geometry independently of the Rust parser.
const imported = core.importNavMesh(bytes);
const [positions, indices] = core.getNavMeshPositionsAndIndices(imported.navMesh);
writeFileSync(new URL('./recast-0.43.1.bin', import.meta.url), bytes);
writeFileSync(new URL('./recast-0.43.1.json', import.meta.url), JSON.stringify({ positions, indices }) + '\n');
result.navMesh.destroy();
imported.navMesh.destroy();
console.log(`Wrote ${bytes.length} bytes and ${indices.length / 3} expected triangles`);
