import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const destination = process.argv[2];
assert.ok(destination, 'Usage: node scripts/export-sdk.mjs NEW_DIRECTORY [--javascript-only]');
assert.ok(process.argv.slice(3).every(arg => arg === '--javascript-only'));
const output = path.resolve(destination);
assert.ok(output !== root && !output.startsWith(`${root}${path.sep}`), 'Export outside the core source');
fs.mkdirSync(output); // Never overwrite a consumer or a previous snapshot.
const roots = process.argv.includes('--javascript-only') ? ['sdk'] :
  ['sdk', 'crates/plugin-protocol', 'crates/plugin-sdk', 'crates/process-engine'];
const files = [];
function copy(relative) {
  const source = path.join(root, relative), stat = fs.lstatSync(source);
  assert.ok(!stat.isSymbolicLink(), `SDK source is a symlink: ${relative}`);
  // Finder metadata is local filesystem state, never a public dependency.
  if (path.basename(relative) === '.DS_Store' && stat.isFile()) return;
  if (stat.isDirectory()) {
    for (const name of fs.readdirSync(source).sort()) {
      if (!['target', 'node_modules', '.git'].includes(name)) copy(`${relative}/${name}`);
    }
  } else {
    assert.ok(stat.isFile());
    const bytes = fs.readFileSync(source), target = path.join(output, relative);
    fs.mkdirSync(path.dirname(target), {recursive: true});
    fs.writeFileSync(target, bytes, {mode: stat.mode & 0o777});
    files.push({path: relative, bytes: bytes.length,
      sha256: createHash('sha256').update(bytes).digest('hex'), executable: Boolean(stat.mode & 0o111)});
  }
}
for (const relative of roots) copy(relative);
// Licenses are part of the dependency, including when it is exported on its own.
fs.copyFileSync(path.join(root, 'LICENSE'), path.join(output, 'CORE-LICENSE'));
const license = fs.readFileSync(path.join(output, 'CORE-LICENSE'));
files.push({path: 'CORE-LICENSE', bytes: license.length,
  sha256: createHash('sha256').update(license).digest('hex'), executable: false});
files.sort((a, b) => a.path.localeCompare(b.path, 'en'));
const git = (...args) => execFileSync('git', args, {cwd: root, encoding: 'utf8'}).trim();
const manifest = {format: 1, source_revision: git('rev-parse', 'HEAD'),
  source_dirty: Boolean(git('status', '--porcelain')), roots,
  sha256: createHash('sha256').update(JSON.stringify(files)).digest('hex'), files};
fs.writeFileSync(path.join(output, 'core-sdk.json'), JSON.stringify(manifest, null, 2) + '\n');
console.log(JSON.stringify({directory: output, revision: manifest.source_revision,
  dirty: manifest.source_dirty, sha256: manifest.sha256}));
