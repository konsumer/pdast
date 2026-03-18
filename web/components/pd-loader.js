/**
 * <pd-loader>
 *
 * Manages a collection of loaded .pd and .json files.  Provides the
 * abstraction loader callback used by the pdast WASM parser, so that
 * any .pd file in the collection can be referenced as an abstraction
 * by any other.
 *
 * Attributes:
 *   wasm-url  — URL to pdast.js (default: resolves ../pdast/pdast.js relative
 *               to this file, which works for the bundled web demo)
 *
 * Properties (read):
 *   patches   — Map<filename, string>   all loaded .pd content
 *   astFiles  — Map<filename, object>   all loaded .json ParseResult objects
 *
 * Methods:
 *   addFiles(FileList | File[])   — load files and parse them
 *   clear()                       — remove all loaded files
 *   getLoader()                   — returns the abstraction loader function
 *                                   (name: string) => string | null
 *
 * Events:
 *   pd-loaded   — fired after each batch; detail = { patches, astFiles, warnings }
 *   pd-error    — fired on parse failure; detail = { file, error }
 *
 * Usage:
 *   <pd-loader id="loader"></pd-loader>
 *   loader.addEventListener('pd-loaded', e => console.log(e.detail));
 *
 * The component renders a <pd-file-drop> zone and a file list.
 * Wrap or extend for custom UI.
 */

import { baseCSS } from './pd-styles.js'
import './pd-file-drop.js'

const css = `
  :host { display: block; }
  .root { display: flex; flex-direction: column; gap: 0.75em; }

  .file-list {
    display: flex;
    flex-direction: column;
    gap: 0.3em;
  }
  .file-item {
    display: flex;
    align-items: center;
    gap: 0.5em;
    padding: 0.35em 0.6em;
    background: var(--pd-surface);
    border: 1px solid var(--pd-border);
    border-radius: var(--pd-radius);
    font-size: 0.85em;
  }
  .file-item .name {
    flex: 1;
    font-family: var(--pd-font);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    cursor: pointer;
    color: var(--pd-accent);
  }
  .file-item .name:hover { text-decoration: underline; }
  .file-item .badge {
    font-size: 0.75em;
    padding: 0.1em 0.4em;
    border-radius: 3px;
    background: var(--pd-obj);
    color: var(--pd-text-dim);
  }
  .file-item .badge.pd  { color: var(--pd-accent); }
  .file-item .badge.ast { color: var(--pd-accent2); }
  .file-item .warn {
    color: var(--pd-warn);
    font-size: 0.8em;
  }
  .file-item button.remove {
    border: none;
    background: none;
    color: var(--pd-text-dim);
    padding: 0 0.2em;
    font-size: 1em;
    line-height: 1;
  }
  .file-item button.remove:hover { color: var(--pd-error); }
  .file-item.active { border-color: var(--pd-accent); background: var(--pd-obj); }
  .toolbar {
    display: flex;
    gap: 0.5em;
    align-items: center;
    flex-wrap: wrap;
  }
  .toolbar span {
    margin-left: auto;
    font-size: 0.8em;
    color: var(--pd-text-dim);
  }
  .empty { color: var(--pd-text-dim); font-size: 0.85em; text-align: center; padding: 0.5em; }
`

class PdLoader extends HTMLElement {
  static observedAttributes = ['wasm-url']

  constructor() {
    super()
    this.attachShadow({ mode: 'open' })
    const sheet = new CSSStyleSheet()
    sheet.replaceSync(baseCSS + css)
    this.shadowRoot.adoptedStyleSheets = [sheet]

    /** @type {Map<string, string>} */
    this.patches = new Map()
    /** @type {Map<string, object>} */
    this.astFiles = new Map()
    /** @type {Map<string, string[]>} */
    this._warnings = new Map()

    this._wasmReady = false
    this._parse = null

    this._initWasm()
    this._render()
    this._bind()
  }

  async _initWasm() {
    try {
      // Use wasm-url attribute if provided, otherwise resolve relative to this
      // file (works for the web demo at any URL prefix).
      const wasmUrl = this.getAttribute('wasm-url') ?? new URL('../pdast/pdast.js', import.meta.url).href
      const mod = await import(wasmUrl)
      // --target web builds require calling the default init() before use
      if (typeof mod.default === 'function') {
        await mod.default()
      }
      this._parse = mod.parse
      this._wasmReady = true
    } catch (e) {
      console.error('[pd-loader] WASM load failed:', e)
    }
  }

  _render() {
    this.shadowRoot.innerHTML = `
      <div class="root">
        <pd-file-drop accept=".pd,.json" multiple
          label="Drop .pd patch files or .json AST files">
        </pd-file-drop>
        <div class="toolbar">
          <button class="clear-btn" title="Remove all files">Clear all</button>
          <span class="status"></span>
        </div>
        <div class="file-list" role="list"></div>
      </div>
    `
  }

  _bind() {
    this.shadowRoot.querySelector('pd-file-drop')?.addEventListener('pd-files', (e) => this.addFiles(e.detail))
    this.shadowRoot.querySelector('.clear-btn')?.addEventListener('click', () => this.clear())
  }

  /** Read a File as text */
  _readFile(file) {
    return new Promise((res, rej) => {
      const r = new FileReader()
      r.onload = () => res(r.result)
      r.onerror = () => rej(r.error)
      r.readAsText(file)
    })
  }

  /**
   * Load and parse a FileList or array of Files.
   * @param {FileList|File[]} files
   */
  async addFiles(files) {
    const arr = Array.from(files)
    const allWarnings = []
    const newlyParsed = []

    for (const file of arr) {
      try {
        const text = await this._readFile(file)
        const name = file.name

        if (name.endsWith('.json')) {
          // Treat as a pre-parsed AST / ParseResult
          const obj = JSON.parse(text)
          // Accept bare Patch or ParseResult wrapper
          const parsed = obj.patch ?? obj
          this.astFiles.set(name, { patch: parsed, warnings: obj.warnings ?? [] })
          if (obj.warnings?.length) {
            this._warnings.set(
              name,
              obj.warnings.map((w) => w.message)
            )
            allWarnings.push(...obj.warnings.map((w) => `${name}: ${w.message}`))
          }
          newlyParsed.push(name)
        } else {
          // Treat as a .pd patch
          // Strip directory prefix if drag-dropped
          const shortName = name.replace(/.*[/\\]/, '')
          this.patches.set(shortName, text)
        }
      } catch (err) {
        this.dispatchEvent(
          new CustomEvent('pd-error', {
            detail: { file: file.name, error: err },
            bubbles: true,
            composed: true
          })
        )
      }
    }

    // Now parse all newly loaded .pd files (so abstractions can cross-reference)
    for (const [name, content] of this.patches) {
      if (this.astFiles.has(name)) continue // already parsed
      if (!this._wasmReady) {
        // Wait up to 3s for WASM
        await new Promise((r) => setTimeout(r, 100))
        if (!this._wasmReady) {
          console.warn('[pd-loader] WASM not ready')
          continue
        }
      }
      try {
        const result = this._parse(content, (n) => this.patches.get(n + '.pd') ?? null)
        this.astFiles.set(name, result)
        newlyParsed.push(name)
        if (result.warnings?.length) {
          this._warnings.set(
            name,
            result.warnings.map((w) => w.message)
          )
          allWarnings.push(...result.warnings.map((w) => `${name}: ${w.message}`))
        }
      } catch (err) {
        this.dispatchEvent(
          new CustomEvent('pd-error', {
            detail: { file: name, error: err },
            bubbles: true,
            composed: true
          })
        )
      }
    }

    this._updateList()
    this.dispatchEvent(
      new CustomEvent('pd-loaded', {
        detail: { patches: this.patches, astFiles: this.astFiles, warnings: allWarnings },
        bubbles: true,
        composed: true
      })
    )

    // Auto-select: fire pd-select for the first newly parsed file so the
    // viewer shows something immediately without requiring a manual click.
    const firstNew = newlyParsed[0]
    if (firstNew) {
      this._selectFile(firstNew)
    }
  }

  /** Remove all loaded files */
  clear() {
    this.patches.clear()
    this.astFiles.clear()
    this._warnings.clear()
    this._updateList()
    this.dispatchEvent(
      new CustomEvent('pd-loaded', {
        detail: { patches: this.patches, astFiles: this.astFiles, warnings: [] },
        bubbles: true,
        composed: true
      })
    )
  }

  /**
   * Select a file by name and emit pd-select.
   * Marks it as active in the list.
   * @param {string} name
   */
  _selectFile(name) {
    const result = this.astFiles.get(name)
    if (!result) return
    this._activeFile = name
    // Update active highlight in list
    this.shadowRoot.querySelectorAll('.file-item').forEach((el) => {
      el.classList.toggle('active', el.querySelector('.name')?.textContent === name)
    })
    this.dispatchEvent(
      new CustomEvent('pd-select', {
        detail: { name, result },
        bubbles: true,
        composed: true
      })
    )
  }

  /**
   * Returns a loader function suitable for passing to the WASM parse() call.
   * @returns {(name: string) => string | null}
   */
  getLoader() {
    return (name) => this.patches.get(name + '.pd') ?? this.patches.get(name) ?? null
  }

  _updateList() {
    const list = this.shadowRoot.querySelector('.file-list')
    const status = this.shadowRoot.querySelector('.status')
    if (!list) return

    const allNames = new Set([...this.patches.keys(), ...this.astFiles.keys()])
    status.textContent = allNames.size ? `${allNames.size} file${allNames.size !== 1 ? 's' : ''} loaded` : ''

    if (!allNames.size) {
      list.innerHTML = '<div class="empty">No files loaded</div>'
      return
    }

    list.innerHTML = ''
    for (const name of [...allNames].sort()) {
      const isPd = this.patches.has(name)
      const isAst = this.astFiles.has(name)
      const warns = this._warnings.get(name) ?? []

      const item = document.createElement('div')
      item.className = 'file-item'
      item.setAttribute('role', 'listitem')
      item.innerHTML = `
        <span class="name" title="${name}" tabindex="0">${name}</span>
        ${isPd ? '<span class="badge pd">.pd</span>' : ''}
        ${isAst ? '<span class="badge ast">AST</span>' : ''}
        ${warns.length ? `<span class="warn" title="${warns.join('\n')}">⚠ ${warns.length}</span>` : ''}
        <button class="remove" title="Remove" aria-label="Remove ${name}">✕</button>
      `

      // Click name → select
      item.querySelector('.name').addEventListener('click', () => this._selectFile(name))
      item.querySelector('.name').addEventListener('keydown', (e) => {
        if (e.key === 'Enter') e.target.click()
      })

      // Remove button
      item.querySelector('.remove').addEventListener('click', () => {
        this.patches.delete(name)
        this.astFiles.delete(name)
        this._warnings.delete(name)
        this._updateList()
      })

      list.appendChild(item)
    }
  }
}

customElements.define('pd-loader', PdLoader)
