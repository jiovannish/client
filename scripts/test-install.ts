import assert from 'node:assert/strict';
import { execFileSync, spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, readdirSync, rmSync, symlinkSync, copyFileSync } from 'node:fs';
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
const { copyFileSync, readFileSync } = require('node:fs');
const args = process.argv.slice(2);
if (process.env.TEST_DOWNLOAD_FAIL) process.exit(22);
for (const flag of ['--fail', '--proto', '--proto-redir', '--max-time']) {
  if (!args.includes(flag)) process.exit(1);
}
const url = args.find(a => a.startsWith('https://'));
if (url === 'https://github.com/jiovannish/client/releases/latest/download/install.sh') {
  if (process.env.TEST_INSTALLER_DOWNLOAD_FAIL) {
    process.stdout.write('echo partial > "$JIO_INSTALL_DIR/partial"');
    process.exit(22);
  }
  process.stdout.write(readFileSync(process.env.TEST_INSTALLER));
  process.exit(0);
}
const base = 'https://github.com/jiovannish/client/releases/download/v0.3.1/';
if (!url?.startsWith(base)) process.exit(1);
const asset = url.slice(base.length);
if (asset !== 'SHA256SUMS' && asset !== 'jio-' + process.env.TEST_TARGET + '.tar.gz') process.exit(1);
copyFileSync(process.env.TEST_FIXTURE + '/' + (asset === 'SHA256SUMS' ? 'checksums' : 'archive'), args[args.indexOf('--output') + 1]);
`, { mode: 0o755 });
  const env: NodeJS.ProcessEnv = { ...process.env, PATH: `${stub}:${process.env.PATH}`, JIO_INSTALL_DIR: bin, TEST_FIXTURE: fixture, TEST_INSTALLER: installer };
  const run = (extra: NodeJS.ProcessEnv = {}) => spawnSync('sh', [], { input: readFileSync(installer, 'utf8'), env: { ...env, ...extra }, encoding: 'utf8' });
  const archive = (version = '0.3.1') => {
    writeFileSync(join(files, 'jio'), `#!/bin/sh\n[ "$1" = --version ] || exit 1\necho 'jio ${version}'\n`, { mode: 0o755 });
    writeFileSync(join(files, 'LICENSE'), 'test license\n');
    writeFileSync(join(files, 'THIRDPARTY.json'), '{}\n');
    execFileSync('tar', ['-czf', join(fixture, 'archive'), '-C', files, 'jio', 'LICENSE', 'THIRDPARTY.json']);
  };
  const checksums = (hash?: string) => writeFileSync(join(fixture, 'checksums'), `${hash ?? createHash('sha256').update(readFileSync(join(fixture, 'archive'))).digest('hex')}  jio-${env.TEST_TARGET}.tar.gz\n`);
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
    assert.equal(execFileSync(join(bin, 'jio'), ['--version'], { encoding: 'utf8' }), 'jio 0.3.1\n');
  }
  // A piped child cannot change its parent's PATH: verify lookup in that parent.
  const home = join(fixture, "user's home");
  const localBin = join(home, '.local', 'bin');
  const parentEnv = { ...env, HOME: home, JIO_INSTALL_DIR: undefined,
    PATH: `${stub}:${localBin}:/usr/bin:/bin:/usr/sbin:/sbin` };
  const parent = spawnSync('sh', ['-ec', 'sh; command -v jio; jio --version'], {
    input: readFileSync(installer, 'utf8'), env: parentEnv, encoding: 'utf8',
  });
  assert.equal(parent.status, 0, parent.stderr);
  assert.ok(parent.stdout.includes(localBin + '/jio'));
  // If no installable directory is on PATH, the printed recovery command must work.
  const fallbackEnv = { ...parentEnv, PATH: `${stub}:/usr/bin:/bin:/usr/sbin:/sbin` };
  const fallback = run(fallbackEnv);
  assert.equal(fallback.status, 0, fallback.stderr);
  const exportLine = fallback.stdout.split('\n').find(line => line.startsWith('  export PATH='));
  assert.ok(exportLine);
  const recovered = spawnSync('sh', ['-ec', exportLine + '; jio --version'], { env: fallbackEnv, encoding: 'utf8' });
  assert.equal(recovered.status, 0, recovered.stderr);
  assert.equal(recovered.stdout, 'jio 0.3.1\n');
  const installed = readFileSync(join(bin, 'jio'));
  const failsSafely = (extra: NodeJS.ProcessEnv = {}) => {
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

  // Run the actual CLI from a custom path. It must replace itself, not a second
  // installation, and never execute a partial installer download or touch state.
  archive(); checksums();
  const updateBin = join(fixture, "user's bin");
  const state = join(fixture, 'state');
  mkdirSync(updateBin); mkdirSync(state);
  writeFileSync(join(state, 'credentials'), 'keep my credentials');
  const updateExe = join(updateBin, 'jio');
  const cli = readFileSync(resolve('target/debug/jio'));
  const update = (extra: NodeJS.ProcessEnv = {}) => spawnSync(updateExe, ['update'], {
    env: { ...env, TEST_OS: 'Darwin', TEST_ARCH: 'arm64', TEST_TARGET: 'aarch64-apple-darwin',
      JIO_ENDPOINT: 'invalid endpoint', JIO_API_KEY: undefined, JIO_STATE_DIR: state, ...extra },
    encoding: 'utf8',
  });
  env.TEST_TARGET = 'aarch64-apple-darwin'; checksums();
  for (const failure of ['TEST_INSTALLER_DOWNLOAD_FAIL', 'TEST_DOWNLOAD_FAIL']) {
    writeFileSync(updateExe, cli, { mode: 0o755 });
    const result = update({ [failure]: '1' });
    assert.notEqual(result.status, 0);
    assert.deepEqual(readFileSync(updateExe), cli);
    assert.deepEqual(readdirSync(updateBin), ['jio']);
  }
  checksums('0'.repeat(64));
  assert.notEqual(update().status, 0);
  assert.deepEqual(readFileSync(updateExe), cli);
  checksums();
  const updated = update();
  assert.equal(updated.status, 0, updated.stderr);
  assert.equal(execFileSync(updateExe, ['--version'], { encoding: 'utf8' }), 'jio 0.3.1\n');
  assert.deepEqual(readFileSync(join(bin, 'jio')), installed);
  assert.equal(readFileSync(join(state, 'credentials'), 'utf8'), 'keep my credentials');
  copyFileSync(resolve('target/debug/jio'), join(updateBin, 'renamed'));
  assert.notEqual(spawnSync(join(updateBin, 'renamed'), ['update']).status, 0);

  rmSync(join(bin, 'jio'));
  symlinkSync(join(files, 'jio'), join(bin, 'jio'));
  assert.notEqual(run().status, 0);
  console.log('Installer checks passed: four targets, CLI self-update, checksums, partial/download failure, version mismatch, paths and symlinks.');
} finally {
  rmSync(fixture, { recursive: true, force: true });
}
