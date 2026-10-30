// Omoikane as a Claude Code mod. Installing the plugin is the whole setup: on the first session this
// module downloads the `omoi` binary that matches the plugin's version (checking its SHA-256) into
// ~/.omoikane/bin, then the model in the background. It serves the memory tools by running
// `omoi tool <name>`, which answers exactly as the MCP server does, and injects the recall on
// session start and on each prompt through `omoi recall`. `/omoikane-web` opens every memory in the browser
// (`omoi serve --open`), and `/omoikane` makes the agent save what the session learned. No shell is involved anywhere, so it works the same on Windows,
// macOS and Linux.
import type { Register } from 'claude-code'

const REPO = 'marcmayol/omoikane'

// What `/omoikane` asks the agent to do.
const SAVE = "Save to Omoikane now, with its `remember` and `update` tools. If the user wrote something after the command, that is what to save: one memory, in their words. Otherwise go over this session and save what is worth knowing next time and is not saved yet: decisions and their reasons (kind decision), non-obvious traps with the files they affect (gotcha), how the user wants things done (preference), steps they repeat (procedure), and how something works (fact). `search` first, and `update` a memory instead of saving a near copy. Then update the project's memory tagged `status` (or save one) with where the work was left: what is done and what is next. Write each memory in the language the user writes to you in. Finish with one short line per memory saved or updated, or say that nothing new was worth saving."


// What `prepare` leaves ready: the binary and the instructions the model reads at session start.
type Listo = { omoi: string; instrucciones: string }

let listo: Promise<Listo | undefined> | undefined

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    const l = await ready($)
    if (l !== undefined) await registerTools($, l)
    await $.command.register({ name: 'omoikane', description: 'Save to Omoikane what this session learned (or what you write after it)', argumentHint: '[what to save]' })
    await $.command.register({ name: 'omoikane-web', description: 'Open the Omoikane web view in the browser: search, filter by project or topic, pin, move, forget', immediate: true })
    return next(e)
  })

  // Saving on demand: the agent goes over the session and saves what is worth knowing next time. The
  // text is the one `omoi setup` writes into the other agents' `/omoikane` (TEXTO_GUARDAR in
  // src/agentes.rs). A command.run hook can't submit a prompt itself (it would wait on its own turn),
  // so a timer does it right after; the prompt runs as a turn of its own once the session is idle.
  on('command.run', { command: 'omoikane' }, ($, e) => {
    const lo = e.args.trim()
    const text = lo === '' ? SAVE : `${SAVE}\n\nWhat the user wrote after the command: ${lo}`
    $.clock.after(0, () => {
      void $.prompt.submit({ text }).catch(() => {})
    })
    return { text: 'Omoikane: saving.' }
  })

  // The web view: `omoi serve --open` starts the local server and opens the browser, or just opens it
  // when a server is already running. The server lives as long as this session.
  on('command.run', { command: 'omoikane-web' }, async ($, e) => {
    const l = await ready($)
    if (l === undefined) return { text: 'Omoikane is not ready: its binary could not be installed. See the toast at session start.' }
    void (async () => {
      try {
        for await (const _ of $.process.spawn({ argv: [l.omoi, 'serve', '--open'] })) {
          // Nothing to read: the loop keeps the server alive.
        }
      } catch {
        // It could not start (the port is taken by something else): `omoi serve` already said why.
      }
    })()
    return { text: 'Omoikane: opening http://127.0.0.1:7438/ in your browser. It stays up while this session is open.' }
  })

  on('classic.SessionStart', async ($, e, next) => {
    const r = await next(e)
    const l = await ready($)
    if (l === undefined) return r
    const recuerdo = await recall($, l.omoi, ['recall', '--session-start'], JSON.stringify(e))
    const textos = [recuerdo, l.instrucciones].filter((t): t is string => t !== undefined && t !== '')
    return { ...r, additionalContext: [...(r.additionalContext ?? []), ...textos] }
  })

  // When the session ends or compacts, set aside from the conversation what may be worth saving; the
  // agent of the next session decides. SessionEnd gets 1.5 s, so it is bounded; it never gets in the way.
  on('classic.SessionEnd', async ($, e, next) => {
    const r = await next(e)
    const l = await ready($)
    if (l !== undefined) await setAside($, l.omoi, JSON.stringify(e))
    return r
  })
  on('classic.PreCompact', async ($, e, next) => {
    const r = await next(e)
    const l = await ready($)
    if (l !== undefined) await setAside($, l.omoi, JSON.stringify(e))
    return r
  })

  on('classic.UserPromptSubmit', async ($, e, next) => {
    const r = await next(e)
    const l = await ready($)
    if (l === undefined) return r
    const recuerdo = await recall($, l.omoi, ['recall'], JSON.stringify(e))
    if (recuerdo === undefined) return r
    return { ...r, additionalContext: [...(r.additionalContext ?? []), recuerdo] }
  })

  // One handler per tool, so each matcher is exact.
  on('tool.call', { tool: 'mcp__omoikane__remember' }, ($, e) => runTool($, 'remember', e))
  on('tool.call', { tool: 'mcp__omoikane__search' }, ($, e) => runTool($, 'search', e))
  on('tool.call', { tool: 'mcp__omoikane__get' }, ($, e) => runTool($, 'get', e))
  on('tool.call', { tool: 'mcp__omoikane__update' }, ($, e) => runTool($, 'update', e))
  on('tool.call', { tool: 'mcp__omoikane__forget' }, ($, e) => runTool($, 'forget', e))
  on('tool.call', { tool: 'mcp__omoikane__history' }, ($, e) => runTool($, 'history', e))
  on('tool.call', { tool: 'mcp__omoikane__context' }, ($, e) => runTool($, 'context', e))
  on('tool.call', { tool: 'mcp__omoikane__recent' }, ($, e) => runTool($, 'recent', e))
}

// Whichever hook comes first starts the preparation (classic.SessionStart can fire before
// session.start); the others wait for the same one.
function ready($: any): Promise<Listo | undefined> {
  if (listo === undefined) listo = prepare($)
  return listo
}

// The binary to use, installing it when it's missing or of another version.
async function prepare($: any): Promise<Listo | undefined> {
  try {
    const manifiesto = JSON.parse(await $.fs.read($.plugin.root + '/.claude-plugin/plugin.json'))
    const version: string = manifiesto.version
    const windows = (await $.env.get('OS')) === 'Windows_NT'
    const home = (windows ? await $.env.get('USERPROFILE') : await $.env.get('HOME')) ?? ''
    const carpeta = home + '/.omoikane/bin'
    const propio = carpeta + (windows ? '/omoi.exe' : '/omoi')

    // OMOIKANE_BIN wins; then our own copy, or an `omoi` on the PATH, if it's this version or newer
    // (omoi updates itself once a day: an older plugin must not put the old binary back).
    const elegido = (await $.env.get('OMOIKANE_BIN')) ?? undefined
    let omoi: string | undefined = elegido
    if (omoi === undefined) {
      for (const c of [propio, 'omoi']) {
        if (notOlder(await versionOf($, c), version)) {
          omoi = c
          break
        }
      }
    }
    if (omoi === undefined) {
      $.ui.toast(`Omoikane: installing omoi ${version}…`)
      await install($, version, windows, carpeta)
      if (!notOlder(await versionOf($, propio), version)) throw new Error(`the installed omoi isn't ${version}`)
      omoi = propio
      $.ui.toast(`Omoikane: omoi ${version} installed`)
    }

    // The model (search by meaning) downloads in the background; until then, search by words.
    const estado = await $.process.run([omoi, 'model', 'status', '--json'])
    if (estado.exitCode === 0 && estado.stdout.trim().startsWith('[]')) {
      $.process
        .run([omoi, 'model', 'install'], { timeoutMs: 600_000 })
        .then(
          (r: any) => $.ui.toast(r.exitCode === 0 ? 'Omoikane: model installed, search by meaning is on' : 'Omoikane: the model didn\'t install; search works by words'),
          () => $.ui.toast('Omoikane: the model didn\'t install; search works by words'),
        )
    }

    const lista = await $.process.run([omoi, 'tool'])
    if (lista.exitCode !== 0) throw new Error(lista.stderr.trim())
    const { instructions } = JSON.parse(lista.stdout)
    return { omoi, instrucciones: `<omoikane-instructions>\n${instructions}\n</omoikane-instructions>` }
  } catch (err) {
    $.ui.toast(`Omoikane couldn't start: ${String((err as Error)?.message ?? err).slice(0, 200)}`, { timeoutMs: 15_000 })
    return undefined
  }
}

// Whether `v` is `version` or newer within the same major version (`X.Y.Z`).
function notOlder(v: string | undefined, version: string): boolean {
  if (v === undefined) return false
  const a = v.split('.').map(Number), b = version.split('.').map(Number)
  if (a.length !== 3 || b.length !== 3 || a.some(isNaN) || b.some(isNaN) || a[0] !== b[0]) return v === version
  return a[1] > b[1] || (a[1] === b[1] && a[2] >= b[2])
}

async function versionOf($: any, omoi: string): Promise<string | undefined> {
  try {
    const r = await $.process.run([omoi, '--version'])
    return r.exitCode === 0 ? r.stdout.trim().split(/\s+/)[1] : undefined
  } catch {
    return undefined
  }
}

// Downloads the release package for this machine, checks it against SHA256SUMS and unpacks it.
async function install($: any, version: string, windows: boolean, carpeta: string): Promise<void> {
  let destino: string
  if (windows) {
    destino = 'x86_64-pc-windows-msvc'
  } else {
    const u = await $.process.run(['uname', '-sm'])
    const [so, maquina] = u.stdout.trim().split(/\s+/)
    const tabla: Record<string, string> = {
      'Linux x86_64': 'x86_64-unknown-linux-gnu',
      'Linux aarch64': 'aarch64-unknown-linux-gnu',
      'Linux arm64': 'aarch64-unknown-linux-gnu',
      'Darwin arm64': 'aarch64-apple-darwin',
      'Darwin x86_64': 'x86_64-apple-darwin',
    }
    destino = tabla[`${so} ${maquina}`]
    if (destino === undefined) throw new Error(`no prebuilt omoi for ${so} ${maquina}; install it with: cargo install omoikane`)
  }
  const paquete = `omoi-${destino}.${windows ? 'zip' : 'tar.gz'}`
  const base = `https://github.com/${REPO}/releases/download/v${version}`

  const sumas = await $.http.fetch(`${base}/SHA256SUMS`)
  if (!sumas.ok) throw new Error(`can't download SHA256SUMS (HTTP ${sumas.status})`)
  const esperada = sumas.text
    .split('\n')
    .map((l: string) => l.trim().split(/\s+/))
    .find((p: string[]) => p[1] === paquete)?.[0]
  if (esperada === undefined) throw new Error(`${paquete} isn't in SHA256SUMS`)

  if (windows) await $.process.run(['cmd', '/c', 'mkdir', carpeta.replaceAll('/', '\\')])
  else await $.process.run(['mkdir', '-p', carpeta])
  const archivo = `${carpeta}/${paquete}`
  const bajada = await $.process.run(['curl', '-fsSL', '--retry', '2', '-o', archivo, `${base}/${paquete}`], { timeoutMs: 300_000 })
  if (bajada.exitCode !== 0) throw new Error(`can't download ${paquete}: ${bajada.stderr.trim()}`)

  const real = await sha256($, archivo, windows)
  if (real !== esperada.toLowerCase()) {
    await remove($, archivo, windows)
    throw new Error(`the SHA-256 of ${paquete} doesn't match; nothing was installed`)
  }

  // Windows ships bsdtar, which unpacks zip too; Git's own tar earlier on the PATH doesn't.
  const tar = windows ? ((await $.env.get('SystemRoot')) ?? 'C:\\Windows') + '\\System32\\tar.exe' : 'tar'
  const abrir = await $.process.run([tar, '-xf', archivo, '-C', carpeta])
  await remove($, archivo, windows)
  if (abrir.exitCode !== 0) throw new Error(`can't unpack ${paquete}: ${abrir.stderr.trim()}`)
}

async function sha256($: any, archivo: string, windows: boolean): Promise<string> {
  if (windows) {
    const r = await $.process.run(['certutil', '-hashfile', archivo.replaceAll('/', '\\'), 'SHA256'])
    // The second line holds the hash (older Windows put spaces between the bytes).
    return (r.stdout.split(/\r?\n/)[1] ?? '').replace(/\s+/g, '').toLowerCase()
  }
  for (const orden of [['sha256sum', archivo], ['shasum', '-a', '256', archivo]]) {
    try {
      const r = await $.process.run(orden)
      if (r.exitCode === 0) return r.stdout.trim().split(/\s+/)[0].toLowerCase()
    } catch {
      // Not on this system: try the next one.
    }
  }
  throw new Error('neither sha256sum nor shasum is available to check the download')
}

async function remove($: any, archivo: string, windows: boolean): Promise<void> {
  try {
    if (windows) await $.process.run(['cmd', '/c', 'del', '/q', archivo.replaceAll('/', '\\')])
    else await $.process.run(['rm', '-f', archivo])
  } catch {
    // A leftover package is harmless.
  }
}

// Registers the tools as `omoi tool` lists them, so names, descriptions and schemas have one source.
async function registerTools($: any, l: Listo): Promise<void> {
  const r = await $.process.run([l.omoi, 'tool'])
  for (const t of JSON.parse(r.stdout).tools) {
    const { $schema, ...esquema } = t.inputSchema ?? {}
    await $.tool.register({ name: t.name, description: t.description, inputSchema: { type: 'object', ...esquema } })
  }
}

async function runTool($: any, nombre: string, e: any) {
  const l = await ready($)
  if (l === undefined) return { deny: 'Omoikane is not ready: its binary could not be installed. See the toast at session start.' }
  const { tool, tool_use_id, agentId, ...args } = e
  const r = await $.process.run([l.omoi, 'tool', nombre], { stdin: JSON.stringify(args), env: { OMOIKANE_SOURCE: 'mcp:claude-code' } })
  const texto = (r.stdout.trim() || r.stderr.trim() || '(no output)') as string
  return r.exitCode === 0 ? { result: texto } : { deny: texto }
}

async function setAside($: any, omoi: string, entrada: string): Promise<void> {
  try {
    await $.process.run([omoi, 'recall', '--session-end'], { stdin: entrada, timeoutMs: 1200 })
  } catch {
    // Nothing set aside this time; the session still ends normally.
  }
}

async function recall($: any, omoi: string, argv: string[], entrada: string): Promise<string | undefined> {
  try {
    const r = await $.process.run([omoi, ...argv], { stdin: entrada })
    if (r.exitCode !== 0 || r.stdout.trim() === '') return undefined
    return JSON.parse(r.stdout).hookSpecificOutput?.additionalContext
  } catch {
    return undefined
  }
}
