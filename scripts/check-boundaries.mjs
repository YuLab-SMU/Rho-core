import assert from 'node:assert/strict';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--offline', '--no-deps', '--format-version=1'],
  {cwd: root, encoding: 'utf8'}));
for (const pkg of metadata.packages) for (const dependency of pkg.dependencies) {
  if (dependency.path) assert.ok(dependency.path.startsWith(`${root}${path.sep}`),
    `${pkg.name} reads source outside its core repository`);
  assert.ok(!/^(rho-(r|agent|files|editor|environment|remote|annotation)-(api|owner|engine|backend|store)|rig-core|jet_core)$/.test(dependency.name),
    `${pkg.name} imports a scientific implementation: ${dependency.name}`);
}
console.log('Core dependency closure is contained and has no scientific implementation dependency.');
