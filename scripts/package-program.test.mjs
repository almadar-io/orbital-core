import { test } from 'node:test';
import assert from 'node:assert/strict';
import { PLATFORMS, args, builtBinary } from './package-program.mjs';

test('one cargo target dir holds every platform binary at its target triple', () => {
  assert.equal(builtBinary('/t', 'dist', 'orb-shell-typescript', 'linux-arm64', 'musl'), '/t/aarch64-unknown-linux-musl/dist/orb-shell-typescript');
  assert.equal(builtBinary('/t', 'dist', 'orb-shell-typescript', 'windows-x64', 'musl'), '/t/x86_64-pc-windows-msvc/dist/orb-shell-typescript.exe');
  assert.equal(builtBinary('/t', 'dist', 'orb-shell-typescript', 'darwin-x64', 'musl'), '/t/x86_64-apple-darwin/dist/orb-shell-typescript');
});

test('control: a gnu program reads its Linux binary from the gnu target and leaves the others alone', () => {
  assert.equal(builtBinary('/t', 'dist', 'orb-desktop', 'linux-x64', 'gnu'), '/t/x86_64-unknown-linux-gnu/dist/orb-desktop');
  assert.equal(builtBinary('/t', 'dist', 'orb-desktop', 'darwin-arm64', 'gnu'), '/t/aarch64-apple-darwin/dist/orb-desktop');
});

test('the five platforms keep their npm os and cpu', () => {
  assert.deepEqual(Object.entries(PLATFORMS).map(([name, p]) => [name, p.os, p.cpu]), [
    ['darwin-arm64', 'darwin', 'arm64'],
    ['darwin-x64', 'darwin', 'x64'],
    ['linux-x64', 'linux', 'x64'],
    ['linux-arm64', 'linux', 'arm64'],
    ['windows-x64', 'win32', 'x64'],
  ]);
});

test('arguments come as flag value pairs after the command', () => {
  assert.deepEqual(args(['stage', '--name', 'n', '--from', '/t']), { _: 'stage', name: 'n', from: '/t' });
  assert.throws(() => args(['stage', '--name']), /bad argument --name/);
});
