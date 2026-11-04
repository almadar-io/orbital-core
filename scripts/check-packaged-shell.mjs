#!/usr/bin/env node
/**
 * G-ORB-240: run a PACKAGED shell's `generate` from the npm package layout (bin/<binary> + templates/) and assert its
 * template landed. `orbital_core::renderer_packs::shell_template` falls back to the compile-time CARGO_MANIFEST_DIR
 * templates, which still exist on the build runner, so the check runs from a copy outside the source tree and moves
 * the source tree's `templates/` and `renderers/` aside while it runs. A control run with the packaged templates
 * removed must FAIL, which proves the fallback is not masking a missing template.
 *
 *   node check-packaged-shell.mjs --package <dir/platform> --binary <name[.exe]> --repo <shell repo root> --request <json>
 */
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createHash } from 'node:crypto';

function args(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 2) {
    if (!argv[i].startsWith('--') || argv[i + 1] === undefined) throw new Error(`bad argument ${argv[i]}`);
    out[argv[i].slice(2)] = argv[i + 1];
  }
  for (const key of ['package', 'binary', 'repo', 'request']) if (!out[key]) throw new Error(`--${key} is required`);
  return out;
}

function runGenerate(binary, requestText, outDir) {
  const env = { ...process.env };
  delete env.ORBITAL_SHELLS_DIR;
  return spawnSync(binary, ['generate', '--out', outDir], { input: requestText, encoding: 'utf8', env });
}

function entries(dir) {
  return fs.readdirSync(dir).sort();
}

function templateDirs(packageCopy) {
  const root = path.join(packageCopy, 'templates');
  if (!fs.existsSync(root)) return [];
  return entries(root).filter((n) => !n.startsWith('almadar-renderers-') && fs.statSync(path.join(root, n)).isDirectory());
}

function withSourceAside(repo, body) {
  const aside = [];
  try {
    for (const name of ['templates', 'renderers']) {
      const from = path.join(repo, name);
      if (fs.existsSync(from)) {
        const to = `${from}.aside-g-orb-240`;
        fs.renameSync(from, to);
        aside.push([from, to]);
      }
    }
    return body();
  } finally {
    for (const [from, to] of aside.reverse()) fs.renameSync(to, from);
  }
}

const a = args(process.argv.slice(2));
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'packaged-shell-'));
const packageCopy = path.join(work, 'package');
fs.cpSync(a.package, packageCopy, { recursive: true });
const binary = path.join(packageCopy, 'bin', a.binary);
const requestText = fs.readFileSync(a.request, 'utf8');
const templates = templateDirs(packageCopy);
if (templates.length === 0) throw new Error(`${a.package} ships no templates/<name> directory`);
if (a.binary === 'orb-shell-android' || a.binary === 'orb-shell-android.exe') {
  const nativeTemplate = path.join(packageCopy, 'templates/android-native-gpu/app/build.gradle.kts');
  const bundle = path.join(packageCopy, 'native-gpu/android');
  const contract = JSON.parse(fs.readFileSync(path.join(bundle, 'contract.json'), 'utf8'));
  const manifest = JSON.parse(fs.readFileSync(path.join(bundle, 'manifest.json'), 'utf8'));
  const library = fs.readFileSync(path.join(bundle, contract.abi, contract.library));
  const packageVersion = JSON.parse(fs.readFileSync(path.join(packageCopy, 'package.json'), 'utf8')).version;
  if (!fs.existsSync(nativeTemplate) || manifest.contractVersion !== contract.contractVersion || manifest.abi !== contract.abi
    || manifest.shellVersion !== packageVersion || manifest.featureSet !== contract.productionFeatureSet
    || library.toString('hex', 0, 6) !== '7f454c460201' || library.readUInt16LE(18) !== 183
    || manifest.sha256 !== createHash('sha256').update(library).digest('hex')) {
    throw new Error('packaged Android native GPU template, library, or manifest is invalid');
  }
  console.log('ok: packaged Android native GPU template and arm64 library verified');
}

let failure = null;
withSourceAside(path.resolve(a.repo), () => {
  const out = path.join(work, 'out');
  const run = runGenerate(binary, requestText, out);
  if (run.status !== 0) {
    failure = `generate from the packaged layout failed (exit ${run.status}): ${run.stderr}${run.stdout}`;
    return;
  }
  const produced = new Set(entries(out));
  const landed = templates.filter((t) => entries(path.join(packageCopy, 'templates', t)).every((e) => produced.has(e)));
  if (landed.length === 0) {
    failure = `no packaged template landed in the output; templates ${templates.join(', ')}, output ${[...produced].join(', ')}`;
    return;
  }
  console.log(`ok: ${landed.join(', ')} landed from the packaged layout`);

  fs.rmSync(path.join(packageCopy, 'templates'), { recursive: true, force: true });
  const control = runGenerate(binary, requestText, path.join(work, 'control-out'));
  if (control.status === 0) {
    failure = 'control: generate succeeded with the packaged templates removed, so a source-tree fallback is masking the check';
  } else {
    console.log('ok: control run with the packaged templates removed failed as it must');
  }
});

fs.rmSync(work, { recursive: true, force: true });
if (failure) {
  console.error(failure);
  process.exit(1);
}
