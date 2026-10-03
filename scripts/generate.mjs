import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {syncPluginProtocol} from './plugin-protocol.mjs';
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'rho-sdk-'));
try {
  syncPluginProtocol(root, temp, 'generate');
  const output = path.join(temp, 'client');
  execFileSync('cargo', ['run', '-p', 'rho-contract', '--bin', 'export-client', '--locked', '--', output], {cwd: root, stdio: 'inherit'});
  const names = new Set();
  function copy(directory, relative = '') {
    for (const item of fs.readdirSync(directory, {withFileTypes: true})) {
      const name = path.join(relative, item.name), source = path.join(directory, item.name);
      if (item.isDirectory()) copy(source, name);
      else {
        names.add(name);
        const target = path.join(root, 'sdk/host-client', name);
        fs.mkdirSync(path.dirname(target), {recursive: true});
        fs.writeFileSync(target, fs.readFileSync(source, 'utf8').replace(/[ \t]+$/gm, ''));
      }
    }
  }
  copy(output);
  // This directory contains generated declarations only. Retired contracts
  // must disappear from the published inventory, not survive the next export.
  function prune(directory, relative = '') {
    for (const item of fs.readdirSync(directory, {withFileTypes: true})) {
      const name = path.join(relative, item.name), target = path.join(directory, item.name);
      if (item.isDirectory()) prune(target, name);
      else if (!names.has(name)) fs.unlinkSync(target);
    }
  }
  prune(path.join(root, 'sdk/host-client'));
} finally { fs.rmSync(temp, {recursive: true, force: true}); }
