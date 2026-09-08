import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, readdirSync, rmSync, symlinkSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';

const fixture = mkdtempSync(join(tmpdir(), 'jio-installer-test-'));
const installer = resolve('install.sh');
try {
  const stub = join(fixture, 'stub');
  const files = join(fixture, 'files');
  const bin = join(fixture, 'with spaces', 'bin');
  for (const dir of [stub, files, bin]) mkdirSync(dir, { recursive: true });
  writeFileSync(join(stub, 'uname'), '#!/bin/sh\ncase "$1" in -s) echo "$TEST_OS";; -m) echo "$TEST_ARCH";; esac\n', { mode: 0o755 });
  writeFileSync(join(stub, 'curl'), `#!${process.execPath}
const { copyFileSync } = require('node:fs');
const args = process.argv.slice(2);
if (process.env.TEST_DOWNLOAD_FAIL) process.exit(22);
for (const flag of ['--fail', '--proto', '--proto-redir', '--max-time']) {
  if (!args.includes(flag)) process.exit(1);
}
const url = args.find(a => a.startsWith('https://'));
const base = 'https://github.com/jiovannish/client/releases/download/v0.1.0/';
if (!url?.startsWith(base)) process.exit(1);
const asset = url.slice(base.length);
if (asset !== 'SHA256SUMS' && asset !== 'jio-' + process.env.TEST_TARGET + '.tar.gz') process.exit(1);
copyFileSync(process.env.TEST_FIXTURE + '/' + (asset === 'SHA256SUMS' ? 'checksums' : 'archive'), args[args.indexOf('--output') + 1]);
`, { mode: 0o755 });
  const env = { ...process.env, PATH: `${stub}:${process.env.PATH}`, JIO_INSTALL_DIR: bin, TEST_FIXTURE: fixture };
  const run = (extra = {}) => spawnSync('sh', [installer], { env: { ...env, ...extra }, encoding: 'utf8' });
  const archive = (version = '0.1.0') => {
    writeFileSync(join(files, 'jio'), `#!/bin/sh\n[ "$1" = --version ] || exit 1\necho 'jio ${version}'\n`, { mode: 0o755 });
    writeFileSync(join(files, 'LICENSE'), 'test license\n');
    writeFileSync(join(files, 'THIRDPARTY.json'), '{}\n');
    execFileSync('tar', ['-czf', join(fixture, 'archive'), '-C', files, 'jio', 'LICENSE', 'THIRDPARTY.json']);
  };
  const checksums = (hash) => writeFileSync(join(fixture, 'checksums'), `${hash ?? createHash('sha256').update(readFileSync(join(fixture, 'archive'))).digest('hex')}  jio-${env.TEST_TARGET}.tar.gz\n`);
  archive();
  for (const [os, arch, target] of [
    ['Darwin', 'arm64', 'aarch64-apple-darwin'],
    ['Darwin', 'x86_64', 'x86_64-apple-darwin'],
    ['Linux', 'aarch64', 'aarch64-unknown-linux-gnu'],
    ['Linux', 'x86_64', 'x86_64-unknown-linux-gnu'],
  ]) {
    Object.assign(env, { TEST_OS: os, TEST_ARCH: arch, TEST_TARGET: target });
    checksums();
    const result = run();
    assert.equal(result.status, 0, result.stderr);
    assert.equal(execFileSync(join(bin, 'jio'), ['--version'], { encoding: 'utf8' }), 'jio 0.1.0\n');
  }
  const installed = readFileSync(join(bin, 'jio'));
  const failsSafely = (extra = {}) => {
    const result = run(extra);
    assert.notEqual(result.status, 0, result.stdout);
    assert.deepEqual(readFileSync(join(bin, 'jio')), installed);
    assert.deepEqual(readdirSync(bin), ['jio']);
  };
  checksums('0'.repeat(64)); failsSafely();
  checksums(); failsSafely({ TEST_DOWNLOAD_FAIL: '1' });
  failsSafely({ TEST_OS: 'FreeBSD' });
  failsSafely({ JIO_INSTALL_DIR: 'relative/path' });
  writeFileSync(join(fixture, 'checksums'), ''); failsSafely();
  checksums(); writeFileSync(join(fixture, 'checksums'), readFileSync(join(fixture, 'checksums')).toString().repeat(2)); failsSafely();
  archive('9.9.9'); checksums(); failsSafely();
  rmSync(join(bin, 'jio'));
  symlinkSync(join(files, 'jio'), join(bin, 'jio'));
  assert.notEqual(run().status, 0);
  console.log('Installer checks passed: four targets, updates, checksums, download failure, version mismatch, paths and symlinks.');
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
