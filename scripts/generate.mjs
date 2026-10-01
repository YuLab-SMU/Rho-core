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
  function copy(directory, relative = '') {
    for (const item of fs.readdirSync(directory, {withFileTypes: true})) {
      const name = path.join(relative, item.name), source = path.join(directory, item.name);
      if (item.isDirectory()) copy(source, name);
      else {
        const target = path.join(root, 'sdk/host-client', name);
        fs.mkdirSync(path.dirname(target), {recursive: true});
        fs.writeFileSync(target, fs.readFileSync(source, 'utf8').replace(/[ \t]+$/gm, ''));
      }
    }
  }
  copy(output);
} finally { fs.rmSync(temp, {recursive: true, force: true}); }
