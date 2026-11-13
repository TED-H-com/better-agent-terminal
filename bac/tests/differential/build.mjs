import { spawnSync } from 'node:child_process'
import { mkdirSync, existsSync, writeFileSync, readFileSync, rmSync, symlinkSync, openSync, closeSync } from 'node:fs'
import { resolve, join } from 'node:path'
import { createHash } from 'node:crypto'

function command(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: 'utf8', ...options })
  if (result.error || result.status !== 0) throw new Error(`${command} ${args.join(' ')}: exit ${result.status}: ${result.error || result.stderr || 'see build log'}`)
  return result.stdout?.trim()
}
export function buildPair({ root, scratch, report, base, upstreamSource }) {
  const toolchain = command('rustc', ['--version'])
  const upstreamRepo = upstreamSource || root
  if (upstreamSource && command('git', ['-C', upstreamSource, 'rev-parse', 'HEAD']) !== base.commit) throw new Error('Upstream checkout does not match recorded commit')
  // Never hide source changes behind an old binary. Commit runtime changes first.
  const dirty = command('git', ['-C', root, 'diff', 'HEAD', '--name-only']).split('\n').filter(p => p && !p.startsWith('bac/') && p !== '.github/workflows/bac-differential.yml')
  if (dirty.length) throw new Error(`Commit source changes before building: ${dirty.join(', ')}`)
  const forkSha = command('git', ['-C', root, 'rev-parse', 'HEAD'])
  const builds = []
  for (const [label, repo, sha] of [['upstream', upstreamRepo, base.commit], ['fork', root, forkSha]]) {
    const key = createHash('sha256').update(`${sha}:${base.tag}:${toolchain}:headless-debug-v1`).digest('hex').slice(0, 16)
    const cache = join(scratch, `${label}-${sha}-${key}`), source = join(cache, 'source'), target = join(cache, 'target')
    const binary = join(target, 'debug/bat-server'), receiptPath = join(cache, 'build.json')
    let cached = existsSync(receiptPath) && existsSync(binary)
    const started = Date.now()
    if (!cached) {
      mkdirSync(source, { recursive: true })
      // Archive is bounded to a Git object, with no checkout/config/ref mutations.
      const archive = join(cache, 'source.tar')
      command('git', ['-C', repo, 'archive', '--format=tar', `--output=${archive}`, sha])
      command('tar', ['-xf', archive, '-C', source]); rmSync(archive)
      const log = openSync(join(report, `${label}-build.log`), 'w')
      try {
        // Exactly the upstream release stamping script, only in the archive copy.
        command(process.execPath, ['scripts/build-version.js', '--skip-build'], { cwd: source, env: { ...process.env, VERSION: base.tag }, stdio: ['ignore', log, log] })
        command('cargo', ['build', '--locked', '--no-default-features', '--features', 'headless', '--bin', 'bat-server'], {
          cwd: join(source, 'src-tauri'), env: { ...process.env, CARGO_TARGET_DIR: target, CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS || '2' }, stdio: ['ignore', log, log],
        })
      } finally { closeSync(log) }
      writeFileSync(receiptPath, JSON.stringify({ sha, version: base.tag, toolchain, buildSeconds: (Date.now() - started) / 1000 }, null, 2))
    }
    // Use each source's actual sidecar handlers; local dependencies are already
    // installed. CI installs frozen dependencies separately in each checkout.
    const modules = join(source, 'node-sidecar/node_modules')
    if (!existsSync(modules)) symlinkSync(resolve(repo, 'node-sidecar/node_modules'), modules, 'dir')
    builds.push({ label, binary, source, sha, cached, ...JSON.parse(readFileSync(receiptPath, 'utf8')) })
  }
  return builds
}
