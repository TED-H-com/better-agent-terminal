import { spawn, spawnSync } from 'node:child_process'
import { mkdtemp, mkdir, writeFile, readFile, rm, symlink } from 'node:fs/promises'
import { join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { randomBytes } from 'node:crypto'
import net from 'node:net'
import { setTimeout as delay } from 'node:timers/promises'
import { Client } from './client.mjs'
import { bannerFingerprint, ownedServerIdentity } from './tls-identity.mjs'

export async function startServer(build, scratch) {
  const dir = await mkdtemp(join(scratch, 'run-'))
  const home = join(dir, 'home'), data = join(dir, 'data'), repo = join(dir, 'repository'), bin = join(dir, 'bin')
  for (const p of [home, data, repo, bin]) await mkdir(p)
  // A minimal environment has no inherited provider credentials or real homes.
  const env = { HOME: home, USERPROFILE: home, XDG_CONFIG_HOME: join(home, '.config'), XDG_DATA_HOME: join(home, '.local/share'),
    XDG_CACHE_HOME: join(home, '.cache'), TMPDIR: dir, PATH: `${bin}:/usr/bin:/bin`, LANG: 'C.UTF-8', LC_ALL: 'C.UTF-8',
    GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null',
    BAT_SIDECAR_SCRIPT: fileURLToPath(new URL('./sidecar-fixture.mjs', import.meta.url)),
    DIF_SIDECAR_SOURCE: join(build.source, 'node-sidecar/src/server.mjs'),
  }
  await symlink(process.execPath, join(bin, 'node'))
  const catalog = JSON.parse(await readFile(join(build.source, 'runtime-catalog.json'), 'utf8'))
  const runtime = join(data, 'runtimes/codex', catalog.codex.version, `${process.platform}-${process.arch}`, 'bin')
  await mkdir(runtime, { recursive: true })
  const fake = join(runtime, 'codex')
  await symlink(fileURLToPath(new URL('./codex-fixture.mjs', import.meta.url)), fake)
  env.BAT_CODEX_BIN = fake; env.BAT_PM_CODEX_PATH = fake
  for (const name of ['claude', 'codex']) await symlink(fake, join(bin, name))
  await writeFile(join(data, 'remote-known-clients.json'), JSON.stringify({ knownDeviceIds: Array.from({ length: 70 }, (_, i) => `fixture-c${i}`) }))
  await writeFile(join(repo, 'sample.txt'), 'fixture file\n')
  for (const args of [['init', '-b', 'fixture'], ['add', 'sample.txt'], ['-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '-m', 'Fixture']]) {
    const result = spawnSync('git', args, { cwd: repo, env: { ...env, GIT_AUTHOR_DATE: '2000-01-01T00:00:00Z', GIT_COMMITTER_DATE: '2000-01-01T00:00:00Z' }, encoding: 'utf8' })
    if (result.status !== 0) throw new Error(`Fixture git failed: ${result.stderr}`)
  }
  // Reserve a loopback ephemeral port. Only connect after our child announces
  // that exact port, so a bind race can never probe an unrelated listener.
  const reservation = net.createServer()
  await new Promise((yes, no) => { reservation.once('error', no); reservation.listen(0, '127.0.0.1', yes) })
  const port = reservation.address().port
  await new Promise(resolve => reservation.close(resolve))
  const token = randomBytes(32).toString('hex'), tokenFile = join(dir, 'token')
  await writeFile(tokenFile, token, { mode: 0o600 })
  const child = spawn(build.binary, ['--bind=localhost', `--port=${port}`, `--data-dir=${data}`, `--token-file=${tokenFile}`],
    { cwd: dir, env, detached: true, stdio: ['ignore', 'pipe', 'pipe'] })
  let output = '', banner = '', error, identity
  child.stdout.on('data', chunk => { banner = (banner + chunk).slice(-65536) })
  for (const stream of [child.stdout, child.stderr]) stream.on('data', chunk => { output = (output + chunk).slice(-65536) })
  child.on('error', err => { error = err })
  const clients = []
  let stopped = false
  const server = { dir, repo, home, data, port, token, clients,
    logs: () => output.replaceAll(token, '<token>').replaceAll(dir, '<scratch>'),
    async connect(gzip = true) {
      const c = new Client(port, token, `c${clients.length}`, gzip, identity); clients.push(c)
      await c.connect(); return c
    },
    async stop() {
      if (stopped) return
      stopped = true
      clients.forEach(c => c.stop())
      if (child.pid) {
        try { process.kill(-child.pid, 'SIGTERM') } catch (err) { if (err.code !== 'ESRCH') throw err }
        for (let i = 0; i < 100 && child.exitCode === null && child.signalCode === null; i++) await delay(20)
        try { process.kill(-child.pid, 'SIGKILL') } catch (err) { if (err.code !== 'ESRCH') throw err }
      }
      await rm(dir, { recursive: true, force: true })
    },
  }
  try {
    const deadline = Date.now() + 15000
    while ((!banner.includes(`wss://127.0.0.1:${port}\n`) && !banner.includes(`wss://localhost:${port}\n`)) || !bannerFingerprint.test(banner)) {
      if (error || child.exitCode !== null || child.signalCode !== null || Date.now() > deadline) throw new Error(`Owned server did not announce port: ${error || output.replaceAll(token, '<token>').replaceAll(dir, '<scratch>')}`)
      await delay(20)
    }
    identity = ownedServerIdentity(banner, await readFile(join(data, 'server-cert.enc.json'), 'utf8'))
    return server
  } catch (err) { await server.stop(); throw err }
}
