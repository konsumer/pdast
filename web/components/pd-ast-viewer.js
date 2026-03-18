/**
 * <pd-ast-viewer>
 *
 * Renders a pdast ParseResult (or bare Patch) as a collapsible JSON tree
 * with syntax highlighting and node-type colour coding.
 *
 * Properties:
 *   result   — ParseResult | Patch | null    the AST to display
 *
 * Attributes:
 *   filename — string   shown in the header
 *   expanded — boolean  start fully expanded (default: partially expanded)
 *
 * Events:
 *   pd-node-click  — fired when a node row is clicked;
 *                    detail = { path: string[], value: any }
 *
 * Usage:
 *   <pd-ast-viewer filename="my-patch.pd"></pd-ast-viewer>
 *   viewer.result = parseResult;
 */

import { baseCSS } from './pd-styles.js'

const css = `
  :host {
    display: flex;
    flex-direction: column;
    background: var(--pd-bg);
    border: 1px solid var(--pd-border);
    border-radius: var(--pd-radius);
    overflow: hidden;
    min-height: 0;
  }
  .header {
    display: flex;
    align-items: center;
    gap: 0.5em;
    padding: 0.5em 0.75em;
    background: var(--pd-surface);
    border-bottom: 1px solid var(--pd-border);
    font-size: 0.85em;
    flex-shrink: 0;
  }
  .header .filename {
    font-family: var(--pd-font);
    color: var(--pd-accent);
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .header .counts { color: var(--pd-text-dim); font-size: 0.9em; }
  .header button { font-size: 0.8em; padding: 0.2em 0.5em; }

  .tree-root {
    flex: 1;
    overflow: auto;
    padding: 0.5em 0;
    font-family: var(--pd-font);
    font-size: 0.82em;
    line-height: 1.6;
  }

  .empty {
    padding: 2em;
    text-align: center;
    color: var(--pd-text-dim);
  }

  /* Tree rows */
  .tree-node {
    display: flex;
    align-items: baseline;
    gap: 0.3em;
    padding: 0.05em 0.5em 0.05em calc(0.5em + var(--depth, 0) * 1.2em);
    cursor: default;
    transition: background 0.1s;
    white-space: nowrap;
  }
  .tree-node:hover { background: var(--pd-surface); }
  .tree-node.clickable { cursor: pointer; }
  .tree-node.clickable:hover { background: var(--pd-obj); }

  .toggle {
    display: inline-block;
    width: 1em;
    text-align: center;
    flex-shrink: 0;
    color: var(--pd-text-dim);
    font-style: normal;
    user-select: none;
  }
  .toggle.leaf { opacity: 0; pointer-events: none; }

  .key   { color: var(--pd-accent);  }
  .colon { color: var(--pd-text-dim); margin: 0 0.2em; }

  /* Value types */
  .val-str    { color: #a6e3a1; }
  .val-num    { color: #fab387; }
  .val-bool   { color: #cba6f7; }
  .val-null   { color: var(--pd-text-dim); }
  .val-key    { color: var(--pd-warn); }  /* kind / type discriminants */

  /* Node kind badges */
  .badge {
    font-size: 0.75em;
    padding: 0.05em 0.35em;
    border-radius: 3px;
    margin-left: 0.3em;
    vertical-align: middle;
  }
  .badge-obj   { background: #313244; color: var(--pd-accent); }
  .badge-sig   { background: #1e3a5f; color: #89b4fa; }
  .badge-gui   { background: #3d2d59; color: #cba6f7; }
  .badge-ctrl  { background: #2d3d2d; color: var(--pd-accent2); }
  .badge-conn  { background: #3d2d2d; color: var(--pd-error); }
  .badge-warn  { background: #3d3020; color: var(--pd-warn); }

  .children { display: contents; }
  .children.collapsed { display: none; }

  .bracket { color: var(--pd-text-dim); }
  .ellipsis { color: var(--pd-text-dim); font-style: italic; }
`

/**
 * Classify a NodeKind `kind` string for badge display.
 */
function nodeBadge(kind) {
  if (!kind) return null
  const signalKinds = ['obj', 'sub_patch', 'graph', 'array', 'unknown']
  const guiKinds = ['gui']
  const ctrlKinds = ['msg', 'float_atom', 'symbol_atom', 'text']

  if (guiKinds.includes(kind)) return ['badge-gui', kind]
  if (ctrlKinds.includes(kind)) return ['badge-ctrl', kind]
  if (signalKinds.includes(kind)) return ['badge-obj', kind]
  return null
}

/** Is the node kind a signal (tilde) object? */
function isSignalName(name) {
  return typeof name === 'string' && name.endsWith('~')
}

class PdAstViewer extends HTMLElement {
  static observedAttributes = ['filename', 'expanded']

  constructor() {
    super()
    this.attachShadow({ mode: 'open' })
    const sheet = new CSSStyleSheet()
    sheet.replaceSync(baseCSS + css)
    this.shadowRoot.adoptedStyleSheets = [sheet]
    this._result = null
    this._expandedPaths = new Set() // paths that are open
    this._render()
  }

  attributeChangedCallback() {
    this._render()
  }

  get filename() {
    return this.getAttribute('filename') ?? '(untitled)'
  }

  /** @param {object|null} val */
  set result(val) {
    this._result = val
    this._expandedPaths = new Set(['', 'patch', 'patch.root', 'patch.root.nodes'])
    if (this.hasAttribute('expanded')) this._expandAll(val, '', 0)
    this._render()
  }
  get result() {
    return this._result
  }

  _expandAll(obj, path, depth) {
    if (depth > 8 || obj === null || obj === undefined || typeof obj !== 'object') return
    this._expandedPaths.add(path)
    if (Array.isArray(obj)) {
      obj.forEach((item, i) => this._expandAll(item, `${path}[${i}]`, depth + 1))
    } else {
      for (const k of Object.keys(obj)) {
        this._expandAll(obj[k], path ? `${path}.${k}` : k, depth + 1)
      }
    }
  }

  _render() {
    const root = this._result?.patch?.root ?? this._result?.root ?? this._result
    const warnings = this._result?.warnings ?? []
    const nodeCount = root?.nodes?.length ?? 0
    const connCount = root?.connections?.length ?? 0

    this.shadowRoot.innerHTML = `
      <div class="header">
        <span class="filename">${this.filename}</span>
        <span class="counts">${nodeCount} nodes · ${connCount} connections${warnings.length ? ` · <span style="color:var(--pd-warn)">⚠ ${warnings.length}</span>` : ''}</span>
        <button class="expand-all">Expand all</button>
        <button class="collapse-all">Collapse</button>
      </div>
      <div class="tree-root" role="tree"></div>
    `

    this.shadowRoot.querySelector('.expand-all').onclick = () => {
      this._expandAll(this._result, '', 0)
      this._renderTree()
    }
    this.shadowRoot.querySelector('.collapse-all').onclick = () => {
      this._expandedPaths = new Set([''])
      this._renderTree()
    }

    this._renderTree()
  }

  _renderTree() {
    const treeRoot = this.shadowRoot.querySelector('.tree-root')
    if (!treeRoot) return

    if (!this._result) {
      treeRoot.innerHTML = '<div class="empty">No AST loaded</div>'
      return
    }

    treeRoot.innerHTML = ''
    this._buildNodes(this._result, '', 0, treeRoot, null)
  }

  _buildNodes(obj, path, depth, container, keyLabel) {
    // Treat JS undefined the same as null (serde-wasm-bindgen maps Option::None → undefined)
    if (obj === undefined) obj = null
    if (Array.isArray(obj)) {
      this._buildArray(obj, path, depth, container, keyLabel)
    } else if (obj !== null && typeof obj === 'object') {
      this._buildObject(obj, path, depth, container, keyLabel)
    } else {
      this._buildLeaf(obj, path, depth, container, keyLabel)
    }
  }

  _buildObject(obj, path, depth, container, keyLabel) {
    const keys = Object.keys(obj)
    const isExpandable = keys.length > 0
    const isOpen = this._expandedPaths.has(path)

    // Special: if this object has a string `kind` field, show a badge.
    // Only treat it as a badge discriminant when it's a plain string
    // (not a nested NodeKind object like { kind: "obj", name: "osc~", ... }).
    const kind = typeof obj.kind === 'string' ? obj.kind : null
    const objName = typeof obj.name === 'string' ? obj.name : null

    const row = this._makeRow(depth, keyLabel, isExpandable, isOpen, () => {
      if (isOpen) this._expandedPaths.delete(path)
      else this._expandedPaths.add(path)
      this._renderTree()
    })

    // Inline summary / badges
    const summary = document.createElement('span')
    summary.style.color = 'var(--pd-text-dim)'

    if (kind) {
      const badge = document.createElement('span')
      const [cls] = nodeBadge(kind) ?? ['badge-obj']
      // Colour-code signal vs control for obj nodes
      if (kind === 'obj' && objName) {
        badge.className = `badge ${isSignalName(objName) ? 'badge-sig' : 'badge-ctrl'}`
      } else {
        badge.className = `badge ${cls}`
      }
      badge.textContent = objName ? `${kind}: ${objName}` : kind
      summary.appendChild(badge)
    } else {
      summary.textContent = isOpen ? '{' : `{ ${keys.length} fields }`
    }
    row.appendChild(summary)
    container.appendChild(row)

    if (!isExpandable) return

    const childContainer = document.createElement('div')
    childContainer.className = `children${isOpen ? '' : ' collapsed'}`
    container.appendChild(childContainer)

    if (isOpen) {
      for (const k of keys) {
        const childPath = path ? `${path}.${k}` : k
        this._buildNodes(obj[k] ?? null, childPath, depth + 1, childContainer, k)
      }
    }
  }

  _buildArray(arr, path, depth, container, keyLabel) {
    const isExpandable = arr.length > 0
    const isOpen = this._expandedPaths.has(path)

    const row = this._makeRow(depth, keyLabel, isExpandable, isOpen, () => {
      if (isOpen) this._expandedPaths.delete(path)
      else this._expandedPaths.add(path)
      this._renderTree()
    })

    const summary = document.createElement('span')
    summary.style.color = 'var(--pd-text-dim)'
    summary.textContent = isOpen ? '[' : `[ ${arr.length} items ]`
    row.appendChild(summary)
    container.appendChild(row)

    if (!isExpandable) return

    const childContainer = document.createElement('div')
    childContainer.className = `children${isOpen ? '' : ' collapsed'}`
    container.appendChild(childContainer)

    if (isOpen) {
      arr.forEach((item, i) => {
        this._buildNodes(item, `${path}[${i}]`, depth + 1, childContainer, String(i))
      })
    }
  }

  _buildLeaf(val, path, depth, container, keyLabel) {
    const row = this._makeRow(depth, keyLabel, false, false, null)
    const span = document.createElement('span')

    if (val === null || val === undefined) {
      span.className = 'val-null'
      span.textContent = 'null'
    } else if (typeof val === 'boolean') {
      span.className = 'val-bool'
      span.textContent = String(val)
    } else if (typeof val === 'number') {
      span.className = 'val-num'
      span.textContent = String(val)
    } else {
      // String — special-case 'kind' and 'type' fields
      const isDiscriminant = keyLabel === 'kind' || keyLabel === 'type'
      span.className = isDiscriminant ? 'val-key' : 'val-str'
      span.textContent = isDiscriminant ? val : `"${val}"`
    }

    row.appendChild(span)

    // Make clickable for non-trivial leaf paths
    if (path.includes('nodes') || path.includes('connections')) {
      row.classList.add('clickable')
      row.addEventListener('click', () => {
        this.dispatchEvent(
          new CustomEvent('pd-node-click', {
            detail: { path: path.split('.'), value: val },
            bubbles: true,
            composed: true
          })
        )
      })
    }

    container.appendChild(row)
  }

  _makeRow(depth, keyLabel, expandable, isOpen, toggleFn) {
    const row = document.createElement('div')
    row.className = 'tree-node'
    row.style.setProperty('--depth', depth)
    row.setAttribute('role', expandable ? 'treeitem' : 'none')
    if (expandable) row.setAttribute('aria-expanded', isOpen)

    // Toggle
    const toggle = document.createElement('i')
    toggle.className = `toggle${expandable ? '' : ' leaf'}`
    toggle.setAttribute('aria-hidden', 'true')
    toggle.textContent = expandable ? (isOpen ? '▾' : '▸') : ' '
    if (expandable && toggleFn) {
      toggle.style.cursor = 'pointer'
      toggle.addEventListener('click', (e) => {
        e.stopPropagation()
        toggleFn()
      })
      row.addEventListener('click', toggleFn)
      row.classList.add('clickable')
    }
    row.appendChild(toggle)

    // Key label
    if (keyLabel !== null) {
      const key = document.createElement('span')
      key.className = 'key'
      key.textContent = keyLabel
      const colon = document.createElement('span')
      colon.className = 'colon'
      colon.textContent = ':'
      row.appendChild(key)
      row.appendChild(colon)
    }

    return row
  }
}

customElements.define('pd-ast-viewer', PdAstViewer)
