import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {test} from 'node:test';
import {verifySnapshot} from '../../sdk/verify-snapshot.mjs';

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');

test('SDK exports survive Finder metadata while rejecting changed or unlisted dependencies', t => {
  const temporary = fs.mkdtempSync(path.join(os.tmpdir(), 'rho-sdk-test-'));
  t.after(() => fs.rmSync(temporary, {recursive: true, force: true}));
  const source = path.join(temporary, 'source'), output = path.join(temporary, 'snapshot');
  fs.mkdirSync(path.join(source, 'scripts'), {recursive: true});
  fs.mkdirSync(path.join(source, 'sdk/nested'), {recursive: true});
  fs.copyFileSync(path.join(repository, 'scripts/export-sdk.mjs'), path.join(source, 'scripts/export-sdk.mjs'));
  fs.writeFileSync(path.join(source, 'LICENSE'), 'Fixture license\n');
  fs.writeFileSync(path.join(source, '.gitignore'), '.DS_Store\n');
  fs.writeFileSync(path.join(source, 'sdk/nested/client.ts'), 'export const version = 1;\n');
  const git = (...args) => execFileSync('git', args, {cwd: source, stdio: 'pipe'});
  git('init', '-q', '-b', 'main');
  git('add', '.');
  git('-c', 'user.name=SDK test', '-c', 'user.email=sdk-test@example.invalid', 'commit', '-qm', 'Fixture');
  fs.writeFileSync(path.join(source, 'sdk/.DS_Store'), 'local Finder metadata');
  fs.writeFileSync(path.join(source, 'sdk/nested/.DS_Store'), 'nested Finder metadata');
  execFileSync(process.execPath, [path.join(source, 'scripts/export-sdk.mjs'), output, '--javascript-only'], {stdio: 'pipe'});
  const manifest = verifySnapshot(output);
  assert.equal(manifest.source_dirty, false);
  assert.ok(!manifest.files.some(file => file.path.includes('.DS_Store')));
  assert.equal(fs.existsSync(path.join(output, 'sdk/.DS_Store')), false);

  const finder = path.join(output, 'sdk/.DS_Store');
  fs.writeFileSync(finder, 'new local Finder metadata');
  verifySnapshot(output);
  const extra = path.join(output, 'sdk/unlisted.ts');
  fs.writeFileSync(extra, 'export const unexpected = true;\n');
  assert.throws(() => verifySnapshot(output), /missing or unlisted/);
  fs.unlinkSync(extra);
  const client = path.join(output, 'sdk/nested/client.ts');
  fs.writeFileSync(client, 'export const version = 2;\n');
  assert.throws(() => verifySnapshot(output), /SDK content changed/);
  fs.copyFileSync(path.join(source, 'sdk/nested/client.ts'), client);
  fs.unlinkSync(finder);
  fs.symlinkSync(path.join(source, 'sdk/.DS_Store'), finder);
  assert.throws(() => verifySnapshot(output), /symlink/);
});
