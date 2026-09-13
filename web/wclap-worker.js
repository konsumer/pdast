/**
 * web/wclap-worker.js — turn pdast2wclap-generated C into a loadable WCLAP
 * `.wasm`, entirely in the browser.
 *
 * Runs the same toolchain the native pipeline uses (clang + wasm-ld, WASI
 * libc sysroot) by loading browsercc's WASM build of LLVM. The C is compiled
 * together with the vendored CLAP runtime shim (`vendor/runtime-shim.c` +
 * `vendor/clap/**`), which supplies `clap_entry` and the plugin surface.
 *
 * Protocol (main thread -> worker):
 *   { cSource: string, pluginId: string, pluginName: string }
 *   -> { type: 'progress', message }
 *   -> { type: 'done', wasm: Uint8Array, output: string }  (wasm transferred)
 *   -> { type: 'error', message, output }
 *
 * The generated C alone is not a plugin — linking it without the shim
 * produces a module with no `clap_entry`, so the shim is not optional.
 */

const BROWSERCC = 'https://cdn.jsdelivr.net/npm/browsercc@0.1.1/dist/'

/** Cached toolchain + sysroot; the download (~95 MB) is worth doing once. */
let toolchain = null

const progress = (message) => self.postMessage({ type: 'progress', message })

async function loadToolchain() {
  if (toolchain) return toolchain

  progress('downloading toolchain')
  const [{ default: Clang }, { default: LLD }, { setUpSysroot }] = await Promise.all([import(/* @vite-ignore */ `${BROWSERCC}clang.js`), import(/* @vite-ignore */ `${BROWSERCC}lld.js`), import(/* @vite-ignore */ `${BROWSERCC}index.js`)])

  const response = await fetch(`${BROWSERCC}sysroot.tar`)
  if (!response.ok) throw new Error(`sysroot.tar: HTTP ${response.status}`)
  const sysroot = await response.arrayBuffer()

  toolchain = { Clang, LLD, setUpSysroot, sysroot }
  return toolchain
}

/**
 * Load every vendored file (shim, pd_wclap.h, CLAP headers) into the shape
 * browsercc's `setUpSysroot` expects: absolute path -> bytes.
 */
async function loadVendorFiles() {
  const manifestUrl = new URL('./vendor/manifest.json', import.meta.url)
  const manifest = await (await fetch(manifestUrl)).json()
  const root = new URL('./vendor/', import.meta.url)

  const files = await Promise.all(
    manifest.files.map(async (name) => {
      const text = await (await fetch(new URL(name, root))).text()
      return [`/${name}`, new TextEncoder().encode(text)]
    })
  )

  return Object.fromEntries(files)
}

/**
 * Create a clang/LLD instance with the sysroot + vendored files mounted.
 * Each translation unit gets its own instance: Emscripten's runtime exits
 * after one `callMain`, and reusing an instance crashes the second call.
 */
async function spawn(toolchain, kind, extraFiles) {
  const { Clang, LLD, setUpSysroot, sysroot } = toolchain
  const factory = kind === 'clang' ? Clang : LLD
  let output = ''
  const capture = (line) => {
    output += line + '\n'
  }
  const module = await factory({
    thisProgram: kind === 'lld' ? 'wasm-ld' : kind,
    print: capture,
    printErr: capture
  })
  setUpSysroot(module, sysroot, extraFiles)
  return { module, output: () => output }
}

/**
 * Split a `-###` command line into argv.
 *
 * The driver prints each argument double-quoted, escaping embedded quotes as
 * `\"` — so a `-DNAME="value"` macro (needed for the plugin id/name string
 * literals) survives the round-trip only if the escapes are undone here.
 */
function parseCommandLine(line) {
  const tokens = []
  let current = ''
  let quoted = false
  let escaped = false

  for (const char of line) {
    if (escaped) {
      current += char
      escaped = false
    } else if (char === '\\') {
      escaped = true
    } else if (char === '"') {
      if (quoted) tokens.push(current)
      current = ''
      quoted = !quoted
    } else if (quoted) {
      current += char
    }
  }

  return tokens
}

/** Ask the clang driver (`-###`) what it would run, without running it. */
async function driverInvocation(toolchain, sources, flags) {
  const { module, output } = await spawn(toolchain, 'clang', {})

  // The driver probes for its sysroot, so it needs the paths to exist even
  // though we only want the printed command lines.
  module.FS.mkdirTree('/lib/wasm32-wasi')
  module.FS.writeFile('/lib/wasm32-wasi/crt1-command.o', new Uint8Array(0))
  module.FS.writeFile('/lib/wasm32-wasi/crt1-reactor.o', new Uint8Array(0))
  module.FS.mkdirTree('/src')
  for (const source of sources) module.FS.writeFile(source, 'int _pd_probe;\n')

  const ret = module.callMain([...sources, ...flags, '-###'])
  if (ret !== 0) throw new Error(`clang driver failed:\n${output()}`)

  // Command lines look like: ` "" "-cc1" "-arg" ...` (the cc1 job has an empty
  // argv[0] placeholder) or ` "wasm-ld" "-arg" ...` (the linker names itself).
  // The empty placeholder stays in `args` — cc1 wants it as argv[0].
  const commands = output()
    .split('\n')
    .filter((line) => line.startsWith(' "'))
    .map((line) => {
      const tokens = parseCommandLine(line)
      return { tool: tokens[0] || tokens[1], args: tokens.slice(1) }
    })

  const jobs = commands.filter(({ args }) => args.includes('-cc1')).map(({ args }) => args)
  const link = commands.find(({ tool }) => tool === 'wasm-ld')?.args
  if (!jobs.length || !link) {
    throw new Error(`could not determine build plan:\n${output()}`)
  }

  return { jobs, link }
}

async function compileWclap({ cSource, pluginId, pluginName }) {
  const toolchain = await loadToolchain()

  progress('loading shim')
  const vendorFiles = await loadVendorFiles()

  const sources = ['/src/pd.c', '/runtime-shim.c']
  const extraFiles = {
    ...vendorFiles,
    '/src/pd.c': new TextEncoder().encode(cSource)
  }

  const output = '/out/plugin.wasm'
  const flags = [
    '--target=wasm32-wasi',
    '-mexec-model=reactor',
    '-O2',
    '-I/',
    // Plugin id/name are printf'd into the CLAP plugin descriptor.
    `-DPD_PLUGIN_ID=${JSON.stringify(pluginId)}`,
    `-DPD_PLUGIN_NAME=${JSON.stringify(pluginName)}`,
    '-Wl,--export=clap_entry',
    '-Wl,--export=malloc',
    '-Wl,--export-table',
    '-Wl,--growable-table',
    '-o',
    output
  ]

  progress('parsing C')
  const { jobs, link } = await driverInvocation(toolchain, sources, flags)

  progress(`compiling ${jobs.length} translation units`)
  let outputText = ''
  const objects = []
  for (const args of jobs) {
    const job = await spawn(toolchain, 'clang', extraFiles)
    const ret = job.module.callMain(args)
    outputText += job.output()
    if (ret !== 0) throw new Error(`compilation failed:\n${outputText}`)

    const name = args[args.indexOf('-o') + 1]
    objects.push({ name, bytes: job.module.FS.readFile(name, { encoding: 'binary' }) })
  }

  progress('linking')
  const linker = await spawn(toolchain, 'lld', extraFiles)
  linker.module.FS.mkdirTree(output.split('/').slice(0, -1).join('/'))
  for (const { name, bytes } of objects) {
    linker.module.FS.mkdirTree(name.split('/').slice(0, -1).join('/'))
    linker.module.FS.writeFile(name, bytes)
  }

  const ret = linker.module.callMain(link)
  outputText += linker.output()
  if (ret !== 0) throw new Error(`link failed:\n${outputText}`)

  const wasm = linker.module.FS.readFile(output, { encoding: 'binary' })
  return { wasm, output: outputText }
}

self.addEventListener('message', async (event) => {
  try {
    const { wasm, output } = await compileWclap(event.data)
    self.postMessage({ type: 'done', wasm, output }, [wasm.buffer])
  } catch (error) {
    self.postMessage({ type: 'error', message: error?.message ?? String(error) })
  }
})
