// Omoikane as a Claude Code mod. Installing the plugin is the whole setup: on the first session this
// module downloads the `omoi` binary that matches the plugin's version (checking its SHA-256) into
// ~/.omoikane/bin, then the model in the background. It serves the memory tools by running
// `omoi tool <name>`, which answers exactly as the MCP server does, and injects the recall on
// session start and on each prompt through `omoi recall`. `/omoikane-web` opens every memory in the browser
// (`omoi serve --open`), and `/omoikane` makes the agent save what the session learned. No shell is involved anywhere, so it works the same on Windows,
// macOS and Linux.
import type { Register } from 'claude-code'

import { register as registerNames } from './nombres'

const REPO = 'marcmayol/omoikane'

// What `/omoikane` asks the agent to do.
const SAVE = "Save to Omoikane now, with its `remember` and `update` tools. If the user wrote something after the command, that is what to save: one memory, in their words. Otherwise go over this session and save what is worth knowing next time and is not saved yet: decisions and their reasons (kind decision), non-obvious traps with the files they affect (gotcha), how the user wants things done (preference), steps they repeat (procedure), and how something works (fact); a decision that is the user's to make and still unanswered goes as kind decision with the tag `open` (the question, the options, what you assumed meanwhile), plus the tag `blocking` if the work can't go on without the answer; a trap of the stack itself that would help any project of the same trade (android, web, wordpress…), with nothing of this project or its client in it, goes with `trade` instead of in the project. `search` first, and `update` a memory instead of saving a near copy. Then update the project's memory tagged `status` (or save one) with where the work was left: what is done and what is next. Write each memory in the language the user writes to you in. Finish with one short line per memory saved or updated, or say that nothing new was worth saving."


// What `prepare` leaves ready: the binary and the instructions the model reads at session start.
type Ready = { omoi: string; instructionsBlock: string }

let readyPromise: Promise<Ready | undefined> | undefined

// The tools that open a file, and the files already looked up in this session (`classic.PreToolUse`).
const OPENERS = ['Read', 'Edit', 'Write', 'MultiEdit', 'NotebookEdit']
const openedFiles = new Set<string>()
let seenSession: string | undefined

export const register: Register = on => {
  // Subagents named "Project: task" (it was the agent-names plugin): see nombres.ts.
  registerNames(on)
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
    const recalled = await recall($, l.omoi, ['recall', '--session-start'], JSON.stringify(e))
    const texts = [recalled, l.instructionsBlock].filter((t): t is string => t !== undefined && t !== '')
    return { ...r, additionalContext: [...(r.additionalContext ?? []), ...texts] }
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
    const recalled = await recall($, l.omoi, ['recall'], JSON.stringify(e))
    if (recalled === undefined) return r
    return { ...r, additionalContext: [...(r.additionalContext ?? []), recalled] }
  })

  // Before Read, Edit or Write opens a file: the memories tied to that file by their paths, even if the
  // prompt never named it (`omoi recall --file-open`). Once per file and session, asked here first so
  // reopening a file costs nothing; `omoi` also skips memories this session already got.
  on('classic.PreToolUse', async ($, e, next) => {
    const r = await next(e)
    if (r.deny !== undefined || !OPENERS.includes(e.tool)) return r
    const args = e as unknown as Record<string, unknown>
    const file = [args.file_path, args.notebook_path].find((f): f is string => typeof f === 'string' && f !== '')
    if (file === undefined) return r
    const session: string = await $.session.id()
    if (session !== seenSession) {
      seenSession = session
      openedFiles.clear()
    }
    if (openedFiles.has(file)) return r
    openedFiles.add(file)
    const l = await ready($)
    if (l === undefined) return r
    const input = JSON.stringify({ session_id: session, tool_name: e.tool, tool_input: { file_path: file } })
    const recalled = await recall($, l.omoi, ['recall', '--file-open'], input)
    if (recalled === undefined) return r
    return { ...r, additionalContext: [...(r.additionalContext ?? []), recalled] }
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
  on('tool.call', { tool: 'mcp__omoikane__skill' }, ($, e) => runTool($, 'skill', e))
  on('tool.call', { tool: 'mcp__omoikane__decisions' }, ($, e) => runTool($, 'decisions', e))
}

// Whichever hook comes first starts the preparation (classic.SessionStart can fire before
// session.start); the others wait for the same one.
function ready($: any): Promise<Ready | undefined> {
  if (readyPromise === undefined) readyPromise = prepare($)
  return readyPromise
}

// The binary to use, installing it when it's missing or of another version.
async function prepare($: any): Promise<Ready | undefined> {
  try {
    const manifest = JSON.parse(await $.fs.read($.plugin.root + '/.claude-plugin/plugin.json'))
    const version: string = manifest.version
    const windows = (await $.env.get('OS')) === 'Windows_NT'
    const home = (windows ? await $.env.get('USERPROFILE') : await $.env.get('HOME')) ?? ''
    const binDir = home + '/.omoikane/bin'
    const own = binDir + (windows ? '/omoi.exe' : '/omoi')

    // OMOIKANE_BIN wins; then our own copy, or an `omoi` on the PATH, if it's this version or newer
    // (omoi updates itself once a day: an older plugin must not put the old binary back).
    const chosen = (await $.env.get('OMOIKANE_BIN')) ?? undefined
    let omoi: string | undefined = chosen
    if (omoi === undefined) {
      for (const c of [own, 'omoi']) {
        if (notOlder(await versionOf($, c), version)) {
          omoi = c
          break
        }
      }
    }
    if (omoi === undefined) {
      $.ui.toast(`Omoikane: installing omoi ${version}…`)
      await install($, version, windows, binDir)
      if (!notOlder(await versionOf($, own), version)) throw new Error(`the installed omoi isn't ${version}`)
      omoi = own
      $.ui.toast(`Omoikane: omoi ${version} installed`)
    }

    // The model (search by meaning) downloads in the background; until then, search by words.
    const status = await $.process.run([omoi, 'model', 'status', '--json'])
    if (status.exitCode === 0 && status.stdout.trim().startsWith('[]')) {
      $.process
        .run([omoi, 'model', 'install'], { timeoutMs: 600_000 })
        .then(
          (r: any) => $.ui.toast(r.exitCode === 0 ? 'Omoikane: model installed, search by meaning is on' : 'Omoikane: the model didn\'t install; search works by words'),
          () => $.ui.toast('Omoikane: the model didn\'t install; search works by words'),
        )
    }
    // The meaning model (EmbeddingGemma, ~450 MB) is not downloaded here: omoi fetches it by itself,
    // in the background, once the memory is large enough to need it (or after
    // `omoi model semantic --on`), and says so at the start of that session.

    const list = await $.process.run([omoi, 'tool'])
    if (list.exitCode !== 0) throw new Error(list.stderr.trim())
    const { instructions } = JSON.parse(list.stdout)
    return { omoi, instructionsBlock: `<omoikane-instructions>\n${instructions}\n</omoikane-instructions>` }
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
async function install($: any, version: string, windows: boolean, binDir: string): Promise<void> {
  let target: string
  if (windows) {
    target = 'x86_64-pc-windows-msvc'
  } else {
    const u = await $.process.run(['uname', '-sm'])
    const [os, machine] = u.stdout.trim().split(/\s+/)
    const table: Record<string, string> = {
      'Linux x86_64': 'x86_64-unknown-linux-gnu',
      'Linux aarch64': 'aarch64-unknown-linux-gnu',
      'Linux arm64': 'aarch64-unknown-linux-gnu',
      'Darwin arm64': 'aarch64-apple-darwin',
      'Darwin x86_64': 'x86_64-apple-darwin',
    }
    target = table[`${os} ${machine}`]
    if (target === undefined) throw new Error(`no prebuilt omoi for ${os} ${machine}; install it with: cargo install omoikane`)
  }
  const pkg = `omoi-${target}.${windows ? 'zip' : 'tar.gz'}`
  const base = `https://github.com/${REPO}/releases/download/v${version}`

  const sums = await withTimeout($.http.fetch(`${base}/SHA256SUMS`), 60_000, "SHA256SUMS didn't arrive in 60 s")
  if (!sums.ok) throw new Error(`can't download SHA256SUMS (HTTP ${sums.status})`)
  const expected = sums.text
    .split('\n')
    .map((l: string) => l.trim().split(/\s+/))
    .find((p: string[]) => p[1] === pkg)?.[0]
  if (expected === undefined) throw new Error(`${pkg} isn't in SHA256SUMS`)

  if (windows) await $.process.run(['cmd', '/c', 'mkdir', binDir.replaceAll('/', '\\')])
  else await $.process.run(['mkdir', '-p', binDir])
  const file = `${binDir}/${pkg}`
  await download($, `${base}/${pkg}`, file, windows)

  const actual = await sha256($, file, windows)
  if (actual !== expected.toLowerCase()) {
    await remove($, file, windows)
    throw new Error(`the SHA-256 of ${pkg} doesn't match; nothing was installed`)
  }

  // Windows ships bsdtar, which unpacks zip too; Git's own tar earlier on the PATH doesn't.
  const tar = windows ? ((await $.env.get('SystemRoot')) ?? 'C:\\Windows') + '\\System32\\tar.exe' : 'tar'
  const unpack = await $.process.run([tar, '-xf', file, '-C', binDir])
  await remove($, file, windows)
  if (unpack.exitCode !== 0) throw new Error(`can't unpack ${pkg}: ${unpack.stderr.trim()}`)
}

// Downloads `url` to `file`. On Windows, curl.exe (Schannel) gives up on networks where it can't
// check whether GitHub's certificate was revoked, which is common behind company proxies
// (CRYPT_E_NO_REVOCATION_CHECK): then it tries again with --ssl-revoke-best-effort, which still checks
// but doesn't block when the check can't be done, and last with PowerShell. The SHA-256 is checked
// afterwards either way.
//
// Every attempt has time limits: 30 s to connect, and a transfer that moves under 1 KB/s for 60 s is
// given up (a network that hangs halfway, a proxy that never answers), all within 290 s, before the
// process itself is stopped at 300 s. It downloads to `file.part` and renames it only when it
// arrived whole, so a hang or a cut never leaves a half package under the real name.
async function download($: any, url: string, file: string, windows: boolean): Promise<void> {
  const part = `${file}.part`
  const limits = ['--connect-timeout', '30', '--speed-limit', '1024', '--speed-time', '60', '--max-time', '290']
  const attempts: string[][] = [['curl', '-fsSL', '--retry', '2', ...limits, '-o', part, url]]
  if (windows) {
    attempts.push(['curl', '-fsSL', '--retry', '2', ...limits, '--ssl-revoke-best-effort', '-o', part, url])
    const psPath = part.replaceAll('/', '\\').replaceAll("'", "''")
    attempts.push([
      'powershell', '-NoProfile', '-NonInteractive', '-Command',
      `$ProgressPreference='SilentlyContinue'; [Net.ServicePointManager]::SecurityProtocol=[Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -UseBasicParsing -TimeoutSec 290 -Uri '${url.replaceAll("'", "''")}' -OutFile '${psPath}'`,
    ])
  }
  let error = ''
  for (const cmd of attempts) {
    try {
      const r = await $.process.run(cmd, { timeoutMs: 300_000 })
      if (r.exitCode === 0) {
        const move = windows
          ? await $.process.run(['cmd', '/c', 'move', '/y', part.replaceAll('/', '\\'), file.replaceAll('/', '\\')])
          : await $.process.run(['mv', '-f', part, file])
        if (move.exitCode === 0) return
        error = `can't rename the download: ${(move.stderr || move.stdout || '').trim()}`
      } else {
        error = (r.stderr || r.stdout || '').trim()
      }
    } catch (err) {
      // Stopped at 300 s, or it couldn't run at all.
      error = String((err as Error)?.message ?? err)
    }
    await remove($, part, windows)
  }
  throw new Error(`can't download ${url.split('/').pop()}: ${error.slice(0, 300)}`)
}

// `p`, or an error if it takes longer than `ms`.
async function withTimeout<T>(p: Promise<T>, ms: number, message: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined
  const limit = new Promise<never>((_, reject) => {
    timer = setTimeout(() => reject(new Error(message)), ms)
  })
  try {
    return await Promise.race([p, limit])
  } finally {
    clearTimeout(timer)
  }
}

async function sha256($: any, file: string, windows: boolean): Promise<string> {
  if (windows) {
    const r = await $.process.run(['certutil', '-hashfile', file.replaceAll('/', '\\'), 'SHA256'])
    // The second line holds the hash (older Windows put spaces between the bytes).
    return (r.stdout.split(/\r?\n/)[1] ?? '').replace(/\s+/g, '').toLowerCase()
  }
  for (const cmd of [['sha256sum', file], ['shasum', '-a', '256', file]]) {
    try {
      const r = await $.process.run(cmd)
      if (r.exitCode === 0) return r.stdout.trim().split(/\s+/)[0].toLowerCase()
    } catch {
      // Not on this system: try the next one.
    }
  }
  throw new Error('neither sha256sum nor shasum is available to check the download')
}

async function remove($: any, file: string, windows: boolean): Promise<void> {
  try {
    if (windows) await $.process.run(['cmd', '/c', 'del', '/q', file.replaceAll('/', '\\')])
    else await $.process.run(['rm', '-f', file])
  } catch {
    // A leftover package is harmless.
  }
}

// Registers the tools as `omoi tool` lists them, so names, descriptions and schemas have one source.
async function registerTools($: any, l: Ready): Promise<void> {
  const r = await $.process.run([l.omoi, 'tool'])
  for (const t of JSON.parse(r.stdout).tools) {
    const { $schema, ...schema } = t.inputSchema ?? {}
    await $.tool.register({ name: t.name, description: t.description, inputSchema: { type: 'object', ...schema } })
  }
}

async function runTool($: any, name: string, e: any) {
  const l = await ready($)
  if (l === undefined) return { deny: 'Omoikane is not ready: its binary could not be installed. See the toast at session start.' }
  const { tool, tool_use_id, agentId, ...args } = e
  const r = await $.process.run([l.omoi, 'tool', name], { stdin: JSON.stringify(args), env: { OMOIKANE_SOURCE: 'mcp:claude-code' } })
  const text = (r.stdout.trim() || r.stderr.trim() || '(no output)') as string
  return r.exitCode === 0 ? { result: text } : { deny: text }
}

async function setAside($: any, omoi: string, input: string): Promise<void> {
  try {
    await $.process.run([omoi, 'recall', '--session-end'], { stdin: input, timeoutMs: 1200 })
  } catch {
    // Nothing set aside this time; the session still ends normally.
  }
}

async function recall($: any, omoi: string, argv: string[], input: string): Promise<string | undefined> {
  try {
    const r = await $.process.run([omoi, ...argv], { stdin: input })
    if (r.exitCode !== 0 || r.stdout.trim() === '') return undefined
    return JSON.parse(r.stdout).hookSpecificOutput?.additionalContext
  } catch {
    return undefined
  }
}
