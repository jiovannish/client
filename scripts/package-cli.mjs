// Release-only packaging; the installed CLI does not require Node or Rust.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const target = process.argv[2];
assert.ok(['aarch64-apple-darwin', 'x86_64-apple-darwin', 'aarch64-unknown-linux-gnu', 'x86_64-unknown-linux-gnu', 'x86_64-pc-windows-msvc'].includes(target), 'unsupported target');
const run = (command, args) => execFileSync(command, args, { encoding: 'utf8', maxBuffer: 32 * 1024 * 1024 });
assert.ok(run('rustc', ['-vV']).includes(`host: ${target}\n`), 'release binaries must be built and tested natively');
const metadata = JSON.parse(run('cargo', ['metadata', '--locked', '--no-deps', '--format-version', '1']));
const version = metadata.packages.find(p => p.name === 'jio-cli').version;
const windows = target.endsWith('-windows-msvc');
const executable = windows ? 'jio.exe' : 'jio';
const binary = resolve(`target/${target}/release/${executable}`);
assert.equal(run(binary, ['--version']).trim(), `jio ${version}`);
assert.ok(readFileSync('install.sh', 'utf8').includes(`    version=${version}\n`), 'installer and Cargo version differ');
assert.ok(readFileSync('install.ps1', 'utf8').includes(`    $version = '${version}'`), 'PowerShell installer and Cargo version differ');

const used = new Set(run('cargo', ['tree', '--locked', '-p', 'jio-cli', '--target', target, '--prefix', 'none', '--format', '{p}'])
  .trim().split('\n').map(line => line.match(/^(\S+) v(\S+)/)?.slice(1).join('@')));
const bundle = JSON.parse(run('cargo', ['bundle-licenses', '--format', 'json']));
bundle.root_name = 'jio-cli';
bundle.third_party_libraries = bundle.third_party_libraries.filter(p => used.has(`${p.package_name}@${p.package_version}`));
for (const dependency of bundle.third_party_libraries) {
  assert.ok(dependency.licenses.length > 0, `missing licenses for ${dependency.package_name}`);
  for (const license of dependency.licenses) {
    assert.ok(license.text && license.text !== 'NOT FOUND', `missing license text for ${dependency.package_name}`);
  }
}
const sysroot = run('rustc', ['--print', 'sysroot']).trim();
bundle.rust_standard_library = {
  version: run('rustc', ['--version']).trim(),
  notices_html: readFileSync(join(sysroot, 'share/doc/rust/COPYRIGHT-library.html'), 'utf8'),
};
const directory = resolve(`target/dist/${target}`);
mkdirSync(directory, { recursive: true });
copyFileSync(binary, join(directory, executable));
copyFileSync('LICENSE', join(directory, 'LICENSE'));
writeFileSync(join(directory, 'THIRDPARTY.json'), JSON.stringify(bundle, null, 2) + '\n');
if (windows) {
  execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
    'Compress-Archive -LiteralPath ($env:JIO_PACKAGE_DIR + "/jio.exe"), ($env:JIO_PACKAGE_DIR + "/LICENSE"), ($env:JIO_PACKAGE_DIR + "/THIRDPARTY.json") -DestinationPath $env:JIO_PACKAGE_ZIP -Force'],
  { env: { ...process.env, JIO_PACKAGE_DIR: directory, JIO_PACKAGE_ZIP: resolve(`target/dist/jio-${target}.zip`) }, stdio: 'inherit' });
} else {
  run('tar', ['-czf', `target/dist/jio-${target}.tar.gz`, '-C', directory, executable, 'LICENSE', 'THIRDPARTY.json']);
}
console.log(`Packaged jio ${version} for ${target} with ${bundle.third_party_libraries.length} dependency notices.`);
