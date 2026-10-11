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
 *   node package-program.mjs meta --name orb-shell-python --version 18.0.0 --out <dir> \
 *        --kind shell --shell python --platforms darwin-arm64,darwin-x64,linux-x64,linux-arm64,windows-x64
 */
import fs from 'node:fs';
import path from 'node:path';

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

function platform(a) {
  const name = need(a, 'name');
  const plat = need(a, 'platform');
  const target = PLATFORMS[plat];
  if (!target) throw new Error(`unknown platform ${plat} (expected ${Object.keys(PLATFORMS).join(', ')})`);
  const binary = need(a, 'binary');
  const exe = path.basename(binary);
  const dir = path.join(need(a, 'out'), plat);
  fs.mkdirSync(path.join(dir, 'bin'), { recursive: true });
  fs.copyFileSync(binary, path.join(dir, 'bin', exe));
  fs.chmodSync(path.join(dir, 'bin', exe), 0o755);
  const repo = a.repo;
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

function meta(a) {
  const name = need(a, 'name');
  const version = need(a, 'version');
  const kind = need(a, 'kind');
  if (kind !== 'shell' && kind !== 'desktop') throw new Error(`--kind must be shell or desktop, got ${kind}`);
  const program = kind === 'shell' ? { kind, name: need(a, 'shell') } : { kind };
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
