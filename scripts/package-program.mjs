#!/usr/bin/env node
/**
 * Lay out the npm packages for a program `orb` runs (a shell, orb-desktop):
 *
 *   <out>/<platform>/   @almadar/<name>-<platform>  bin/<binary>[.exe] + the repo's templates
 *   <out>/meta/         @almadar/<name>             orbProgram + optionalDependencies on each platform
 *
 * The platform package carries the binary and, beside it, `templates/<name>/` for
 * every `templates/*` dir of the repo and `templates/almadar-renderers-<shell>` for a
 * renderer pack (`renderers/<x>/package.json`) — where the program looks
 * (`orbital_core::renderer_packs::shell_template`, `<exe>/../templates`).
 *
 * Usage:
 *   node package-program.mjs platform --name orb-shell-python --binary <path> --platform darwin-arm64 \
 *        --version 18.0.0 --repo <shell repo root> --out <dir> [--shell python]
 *   node package-program.mjs meta --name orb-shell-python --version 18.0.0 --repo <shell repo root> --out <dir> \
 *        --kind shell --shell python --platforms darwin-arm64,darwin-x64,linux-x64,linux-arm64,windows-x64
 */
import fs from 'node:fs';
import path from 'node:path';
import { createHash } from 'node:crypto';

const PLATFORMS = {
  'darwin-arm64': { os: 'darwin', cpu: 'arm64' },
  'darwin-x64': { os: 'darwin', cpu: 'x64' },
  'linux-x64': { os: 'linux', cpu: 'x64' },
  'linux-arm64': { os: 'linux', cpu: 'arm64' },
  'windows-x64': { os: 'win32', cpu: 'x64' },
};

function args(argv) {
  const out = { _: argv[0] };
  for (let i = 1; i < argv.length; i += 2) {
    if (!argv[i].startsWith('--') || argv[i + 1] === undefined) throw new Error(`bad argument ${argv[i]}`);
    out[argv[i].slice(2)] = argv[i + 1];
  }
  return out;
}

function need(a, key) {
  if (!a[key]) throw new Error(`--${key} is required for ${a._}`);
  return a[key];
}

function write(file, value) {
  fs.mkdirSync(path.dirname(file), { recursive: true });
  fs.writeFileSync(file, `${JSON.stringify(value, null, 2)}\n`);
}

function copyDir(from, to) {
  fs.cpSync(from, to, {
    recursive: true,
    filter: (p) => !['node_modules', '.git', 'target', '.turbo'].includes(path.basename(p)),
  });
}

function crateVersion(repo) {
  const manifest = fs.readFileSync(path.join(repo, 'Cargo.toml'), 'utf8');
  const section = manifest.split(/^\[package\]\s*$/m)[1]?.split(/^\[/m)[0];
  const version = /^version\s*=\s*"([^"]+)"/m.exec(section ?? '')?.[1];
  if (!version) throw new Error(`${repo}/Cargo.toml has no package version`);
  return version;
}

function platform(a) {
  const name = need(a, 'name');
  const plat = need(a, 'platform');
  const target = PLATFORMS[plat];
  if (!target) throw new Error(`unknown platform ${plat} (expected ${Object.keys(PLATFORMS).join(', ')})`);
  const binary = need(a, 'binary');
  const repo = a.repo;
  if (name === 'orb-shell-android' && crateVersion(need(a, 'repo')) !== need(a, 'version')) {
    throw new Error(`orb-shell-android tag ${a.version} does not match Cargo.toml package version`);
  }
  const exe = path.basename(binary);
  const dir = path.join(need(a, 'out'), plat);
  fs.mkdirSync(path.join(dir, 'bin'), { recursive: true });
  fs.copyFileSync(binary, path.join(dir, 'bin', exe));
  fs.chmodSync(path.join(dir, 'bin', exe), 0o755);
  if (repo) {
    const templates = path.join(repo, 'templates');
    for (const t of fs.existsSync(templates) ? fs.readdirSync(templates) : []) {
      copyDir(path.join(templates, t), path.join(dir, 'templates', t));
    }
    const renderers = path.join(repo, 'renderers');
    for (const r of fs.existsSync(renderers) ? fs.readdirSync(renderers) : []) {
      if (fs.existsSync(path.join(renderers, r, 'package.json'))) {
        copyDir(path.join(renderers, r), path.join(dir, 'templates', `almadar-renderers-${need(a, 'shell')}`));
      }
    }
  }
  if (name === 'orb-shell-android' && !a['android-native-gpu']) {
    throw new Error('orb-shell-android requires --android-native-gpu for a clean-install package');
  }
  if (a['android-native-gpu']) {
    if (name !== 'orb-shell-android') throw new Error('--android-native-gpu is only valid for orb-shell-android');
    const library = fs.readFileSync(a['android-native-gpu']);
    if (library.length < 20 || library.toString('hex', 0, 6) !== '7f454c460201' || library.readUInt16LE(18) !== 183) {
      throw new Error('--android-native-gpu must be an Android arm64 ELF library');
    }
    const libraryDir = path.join(dir, 'native-gpu/android/arm64-v8a');
    fs.mkdirSync(libraryDir, { recursive: true });
    fs.writeFileSync(path.join(libraryDir, 'liborbital_client_dioxus.so'), library);
    write(path.join(dir, 'native-gpu/android/manifest.json'), {
      contractVersion: 1,
      abi: 'arm64-v8a',
      shellVersion: need(a, 'version'),
      featureSet: 'native-a11y',
      sha256: createHash('sha256').update(library).digest('hex'),
    });
    copyDir(path.join(repo, 'renderers/native/template'), path.join(dir, 'templates/android-native-gpu'));
  }
  write(path.join(dir, 'package.json'), {
    name: `@almadar/${name}-${plat}`,
    version: need(a, 'version'),
    description: `${name} for ${plat}`,
    license: 'BUSL-1.1',
    os: [target.os],
    cpu: [target.cpu],
    bin: { [name]: `bin/${exe}` },
    publishConfig: { access: 'public' },
  });
  console.log(`packaged ${dir}`);
}

/** The orb versions the program runs under: the first key of the crate's [package.metadata.orb]; `orb update` installs only a program that declares one. */
function compilerRange(repo) {
  const manifest = fs.readFileSync(path.join(repo, 'Cargo.toml'), 'utf8');
  const found = /\[package\.metadata\.orb\]\ncompiler = "([^"]+)"/.exec(manifest);
  if (!found) throw new Error(`${repo}/Cargo.toml has no [package.metadata.orb] compiler range`);
  return found[1];
}

function meta(a) {
  const name = need(a, 'name');
  const version = need(a, 'version');
  const kind = need(a, 'kind');
  if (kind !== 'shell' && kind !== 'desktop') throw new Error(`--kind must be shell or desktop, got ${kind}`);
  const program = kind === 'shell' ? { kind, name: need(a, 'shell') } : { kind };
  program.compiler = compilerRange(need(a, 'repo'));
  const platforms = need(a, 'platforms').split(',');
  const dir = path.join(need(a, 'out'), 'meta');
  write(path.join(dir, 'package.json'), {
    name: `@almadar/${name}`,
    version,
    description: kind === 'shell' ? `The ${program.name} shell for orb (orb add @almadar/${name}).` : 'The orb desktop program: window, render probe, screenshots.',
    license: 'BUSL-1.1',
    orbProgram: program,
    optionalDependencies: Object.fromEntries(platforms.map((p) => [`@almadar/${name}-${p}`, version])),
    publishConfig: { access: 'public' },
  });
  console.log(`packaged ${dir}`);
}

const a = args(process.argv.slice(2));
if (a._ === 'platform') platform(a);
else if (a._ === 'meta') meta(a);
else throw new Error('usage: package-program.mjs platform|meta …');
