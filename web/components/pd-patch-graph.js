/**
 * <pd-patch-graph>
 *
 * Renders a pdast Canvas as an SVG node-and-wire graph, similar to how
 * PureData displays a patch.  Supports pan and zoom via mouse/touch.
 *
 * Properties:
 *   canvas   — Canvas object (root or sub-patch canvas)
 *   title    — optional string shown in header
 *
 * Events:
 *   pd-node-click  — fired on node click; detail = { node: Node }
 *
 * The graph uses the position data (x, y) from the AST directly, so the
 * layout matches what PureData would show.
 *
 * Pan:  drag the background
 * Zoom: scroll wheel
 */

import { baseCSS } from './pd-styles.js';

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
    padding: 0.4em 0.75em;
    background: var(--pd-surface);
    border-bottom: 1px solid var(--pd-border);
    font-size: 0.82em;
    flex-shrink: 0;
  }
  .title { font-family: var(--pd-font); color: var(--pd-accent); flex: 1; }
  .header button { font-size: 0.8em; padding: 0.15em 0.45em; }
  .svg-wrap {
    flex: 1;
    overflow: hidden;
    cursor: grab;
    position: relative;
  }
  .svg-wrap.panning { cursor: grabbing; }
  svg {
    width: 100%;
    height: 100%;
    display: block;
  }
  .empty {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--pd-text-dim);
    font-size: 0.9em;
  }
`;

// ── SVG helpers ───────────────────────────────────────────────────────────────

const SVG_NS = 'http://www.w3.org/2000/svg';
function el(tag, attrs = {}) {
  const e = document.createElementNS(SVG_NS, tag);
  for (const [k, v] of Object.entries(attrs)) e.setAttribute(k, v);
  return e;
}

// ── Visual constants (plugdata-inspired) ──────────────────────────────────────
const NODE_H    = 20;   // box height px
const NODE_PAD  = 6;    // horizontal text padding px
const FONT_SZ   = 11;   // label font size px
const FONT      = "'JetBrains Mono','Fira Mono',monospace";
const PORT_W    = 6;    // port tab width px
const PORT_H    = 3;    // port tab height px
const PORT_R    = 1;    // port tab corner radius px

// Semantic colours — used for both node borders, port tabs, and wires.
// The same colour always means the same "type" of signal.
const C_SIG   = '#89b4fa';  // audio-rate signal  (blue)
const C_CTRL  = '#a6e3a1';  // control-rate       (green)
const C_BOTH  = '#5ecfcf';  // can carry both     (teal)
const C_MSG   = '#f9e2af';  // message box        (amber)
const C_PATCH = '#cba6f7';  // sub-patch / GUI    (purple)
const C_DIM   = '#585b70';  // text / neutral     (grey)

// Wire stroke widths
const SIG_W  = 2;
const CTRL_W = 1;

/** Estimate node width from label text */
function nodeWidth(label) {
  return Math.max(40, label.length * (FONT_SZ * 0.62) + NODE_PAD * 2);
}

/**
 * Extract a short display label for a node.
 * @param {object} node  AST Node
 */
function nodeLabel(node) {
  const k = node.kind;
  switch (k.kind) {
    case 'obj':        return [k.name, k.args?.map(a => a.value ?? '').join(' ')].filter(Boolean).join(' ');
    case 'msg':        return k.messages?.flat().map(t => t.value ?? '').join(' ') || '(msg)';
    case 'float_atom': return 'float';
    case 'symbol_atom':return 'symbol';
    case 'text':       return k.content?.slice(0, 30) ?? '';
    case 'sub_patch':  return `[pd ${k.name}]`;
    case 'graph':      return '[graph]';
    case 'array':      return `[array ${k.name}]`;
    case 'unknown':    return k.name ?? '(?)';
    case 'gui': {
      const gk = k.gui_kind ?? '?';
      const lbl = k.label ?? '';
      return lbl ? `${gk}: ${lbl}` : gk;
    }
    default: return JSON.stringify(k).slice(0, 30);
  }
}

/**
 * Return the signal type of a node: 'sig', 'ctrl', 'both', 'msg', 'patch', or 'dim'.
 *
 * 'sig'   — audio-rate tilde object (name ends with ~)
 * 'ctrl'  — control-rate object (no tilde)
 * 'both'  — GUI sliders/toggles: they output control but feed into signal paths
 * 'msg'   — message box
 * 'patch' — sub-patch, graph, array
 * 'dim'   — text comment (no colour)
 */
function nodeType(node) {
  const k = node?.kind;
  if (!k) return 'ctrl';
  switch (k.kind) {
    case 'obj':
      return k.name?.endsWith('~') ? 'sig' : 'ctrl';
    case 'msg':
      return 'msg';
    case 'float_atom':
    case 'symbol_atom':
      return 'both';   // atom boxes output numbers that can go either way
    case 'gui':
      return 'both';   // GUI elements control signal parameters
    case 'sub_patch':
    case 'graph':
    case 'array':
      return 'patch';
    case 'text':
      return 'dim';
    default:
      return 'ctrl';
  }
}

/** Map a node type string → the colour used for its border, ports, and wires. */
function typeColor(type) {
  switch (type) {
    case 'sig':   return C_SIG;
    case 'ctrl':  return C_CTRL;
    case 'both':  return C_BOTH;
    case 'msg':   return C_MSG;
    case 'patch': return C_PATCH;
    case 'dim':   return 'transparent';
    default:      return C_DIM;
  }
}

/** Wire stroke-width for a given source node type. */
function wireWidth(type) {
  return type === 'sig' ? SIG_W : CTRL_W;
}

class PdPatchGraph extends HTMLElement {
  static observedAttributes = ['title'];

  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(baseCSS + css);
    this.shadowRoot.adoptedStyleSheets = [sheet];

    this._canvas = null;
    this._title  = '';

    // Pan / zoom state
    this._tx = 20; this._ty = 20; this._scale = 1;
    this._dragging = false;
    this._dragStart = { x: 0, y: 0, tx: 0, ty: 0 };

    this._render();
  }

  attributeChangedCallback(name, _, val) {
    if (name === 'title') { this._title = val; this._render(); }
  }

  /** @param {object|null} val  Canvas AST node */
  set canvas(val) {
    this._canvas = val;
    this._tx = 20; this._ty = 20; this._scale = 1;
    this._render();
  }
  get canvas() { return this._canvas; }

  set title(val) { this._title = val; this._render(); }
  get title()    { return this._title || this.getAttribute('title') || 'patch'; }

  _render() {
    this.shadowRoot.innerHTML = `
      <div class="header">
        <span class="title">${this.title}</span>
        <button class="fit-btn" title="Fit to view">⊡ Fit</button>
        <button class="reset-btn" title="Reset zoom">1:1</button>
      </div>
      <div class="svg-wrap">
        ${this._canvas
          ? `<svg xmlns="${SVG_NS}" role="img" aria-label="PD patch graph"></svg>`
          : '<div class="empty">No patch loaded</div>'}
      </div>
    `;

    this.shadowRoot.querySelector('.fit-btn')?.addEventListener('click', () => this._fitView());
    this.shadowRoot.querySelector('.reset-btn')?.addEventListener('click', () => {
      this._tx = 20; this._ty = 20; this._scale = 1; this._drawGraph();
    });

    if (this._canvas) {
      this._setupPanZoom();
      this._drawGraph();
    }
  }

  _setupPanZoom() {
    const wrap = this.shadowRoot.querySelector('.svg-wrap');
    if (!wrap) return;

    wrap.addEventListener('mousedown', e => {
      if (e.target.closest('[data-node]')) return;
      this._dragging = true;
      wrap.classList.add('panning');
      this._dragStart = { x: e.clientX, y: e.clientY, tx: this._tx, ty: this._ty };
      e.preventDefault();
    });

    window.addEventListener('mousemove', e => {
      if (!this._dragging) return;
      this._tx = this._dragStart.tx + (e.clientX - this._dragStart.x);
      this._ty = this._dragStart.ty + (e.clientY - this._dragStart.y);
      this._updateTransform();
    });

    window.addEventListener('mouseup', () => {
      this._dragging = false;
      wrap.classList.remove('panning');
    });

    wrap.addEventListener('wheel', e => {
      e.preventDefault();
      const factor = e.deltaY < 0 ? 1.1 : 0.9;
      const rect = wrap.getBoundingClientRect();
      const mx = e.clientX - rect.left;
      const my = e.clientY - rect.top;
      this._tx = mx - factor * (mx - this._tx);
      this._ty = my - factor * (my - this._ty);
      this._scale *= factor;
      this._scale = Math.min(4, Math.max(0.1, this._scale));
      this._updateTransform();
    }, { passive: false });
  }

  _updateTransform() {
    const g = this.shadowRoot.querySelector('svg > g.viewport');
    if (g) g.setAttribute('transform', `translate(${this._tx},${this._ty}) scale(${this._scale})`);
  }

  _drawGraph() {
    const svg = this.shadowRoot.querySelector('svg');
    if (!svg || !this._canvas) return;
    svg.innerHTML = '';

    const viewport = el('g', {
      class: 'viewport',
      transform: `translate(${this._tx},${this._ty}) scale(${this._scale})`,
    });
    svg.appendChild(viewport);

    const nodes = this._canvas.nodes       ?? [];
    const conns = this._canvas.connections ?? [];
    const nodeById = new Map(nodes.map(n => [n.id, n]));

    // ── Pre-compute port counts from connections ───────────────────────────────
    // We also gather per-node signal-ness: a node is "signal" if its name ends
    // with ~ or it is a GUI element.
    const outletMax = new Map(); // id → highest outlet index seen
    const inletMax  = new Map(); // id → highest inlet index seen
    for (const c of conns) {
      outletMax.set(c.src_node, Math.max(outletMax.get(c.src_node) ?? 0, c.src_outlet));
      inletMax.set(c.dst_node,  Math.max(inletMax.get(c.dst_node)  ?? 0, c.dst_inlet));
    }

    // Helper: number of outlets/inlets for a node (at least 1 if it appears in conns)
    const nOut = id => (outletMax.has(id) ? outletMax.get(id) + 1 : 1);
    const nIn  = id => (inletMax.has(id)  ? inletMax.get(id)  + 1 : 1);

    // Build geometry map
    const geom = new Map(); // id → {x,y,w,h}

    // Port tab x-centre for a given port index and count across a node of width w
    const portX = (idx, count, w) => (idx + 0.5) / count * w;

    // ── Wires (drawn below nodes) ─────────────────────────────────────────────
    const wireLayer = el('g', { class: 'wires' });
    viewport.appendChild(wireLayer);

    // ── Nodes ─────────────────────────────────────────────────────────────────
    const nodeLayer = el('g', { class: 'nodes' });
    viewport.appendChild(nodeLayer);

    for (const node of nodes) {
      const label  = nodeLabel(node);
      const w      = nodeWidth(label);
      const h      = NODE_H;
      const x      = node.x ?? 0;
      const y      = node.y ?? 0;
      geom.set(node.id, { x, y, w, h });

      const kind   = node.kind?.kind ?? '';
      const type   = nodeType(node);
      const color  = typeColor(type);
      const isText = kind === 'text';

      const g = el('g', {
        class: 'node',
        'data-node': node.id,
        transform: `translate(${x},${y})`,
        style: 'cursor:pointer',
        role: 'button',
        'aria-label': label,
        tabindex: '0',
      });

      // ── Box ────────────────────────────────────────────────────────────────
      if (!isText) {
        if (kind === 'msg') {
          g.appendChild(el('polygon', {
            points: `4,0 ${w},0 ${w},${h} 4,${h} 0,${h / 2}`,
            fill: 'var(--pd-obj)', stroke: color, 'stroke-width': 1,
          }));
        } else {
          g.appendChild(el('rect', {
            width: w, height: h, rx: 2,
            fill: 'var(--pd-obj)', stroke: color, 'stroke-width': 1,
          }));
        }
      }

      // ── Label ──────────────────────────────────────────────────────────────
      const textEl = el('text', {
        x: isText ? 0 : NODE_PAD,
        y: h / 2 + FONT_SZ * 0.37,
        'font-size': FONT_SZ,
        'font-family': FONT,
        fill: isText ? 'var(--pd-text-dim)' : 'var(--pd-text)',
        'font-style': isText ? 'italic' : 'normal',
      });
      textEl.textContent = label;
      g.appendChild(textEl);

      // ── Port tabs ──────────────────────────────────────────────────────────
      // Port tabs use the same colour as the node border so the visual
      // language is consistent: "blue node → blue ports → blue wires".
      if (!isText) {
        const ni = nIn(node.id);
        const no = nOut(node.id);

        // Inlet tabs (top edge, protruding upward)
        for (let i = 0; i < ni; i++) {
          const cx = portX(i, ni, w);
          g.appendChild(el('rect', {
            x: cx - PORT_W / 2, y: -PORT_H,
            width: PORT_W, height: PORT_H, rx: PORT_R,
            fill: color,
          }));
        }
        // Outlet tabs (bottom edge, protruding downward)
        for (let i = 0; i < no; i++) {
          const cx = portX(i, no, w);
          g.appendChild(el('rect', {
            x: cx - PORT_W / 2, y: h,
            width: PORT_W, height: PORT_H, rx: PORT_R,
            fill: color,
          }));
        }
      }

      g.addEventListener('click', () => {
        this.dispatchEvent(new CustomEvent('pd-node-click', {
          detail: { node }, bubbles: true, composed: true,
        }));
      });

      nodeLayer.appendChild(g);
    }

    // ── Draw wires ────────────────────────────────────────────────────────────
    for (const conn of conns) {
      const src = geom.get(conn.src_node);
      const dst = geom.get(conn.dst_node);
      if (!src || !dst) continue;

      const srcNode = nodeById.get(conn.src_node);
      const srcType = nodeType(srcNode);
      const color   = typeColor(srcType);
      const sw      = wireWidth(srcType);

      // Wire starts at outlet tab centre (bottom of source)
      const x1 = src.x + portX(conn.src_outlet, nOut(conn.src_node), src.w);
      const y1 = src.y + src.h + PORT_H; // just below the outlet tab

      // Wire ends at inlet tab centre (top of destination)
      const x2 = dst.x + portX(conn.dst_inlet, nIn(conn.dst_node), dst.w);
      const y2 = dst.y - PORT_H; // just above the inlet tab

      // Path: straight line for same-x, otherwise a short cubic bezier.
      // Keep it minimal — plugdata style is nearly straight.
      const dy   = y2 - y1;
      const bend = dy > 0
        ? Math.min(dy * 0.35, 40)   // forward: small downward S-curve
        : Math.abs(dy) * 0.5 + 30; // backward: loop around

      const d = dy > 0
        ? `M${x1},${y1} C${x1},${y1 + bend} ${x2},${y2 - bend} ${x2},${y2}`
        : `M${x1},${y1} C${x1},${y1 + bend} ${x2},${y2 + bend} ${x2},${y2}`;

      wireLayer.appendChild(el('path', {
        d,
        fill: 'none',
        stroke: color,
        'stroke-width': sw,
        opacity: 0.9,
      }));
    }

    this._fitView();
  }

  _fitView() {
    const svg  = this.shadowRoot.querySelector('svg');
    const wrap = this.shadowRoot.querySelector('.svg-wrap');
    if (!svg || !wrap || !this._canvas?.nodes?.length) return;

    const nodes = this._canvas.nodes;
    let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
    for (const n of nodes) {
      const label = nodeLabel(n);
      const w = nodeWidth(label);
      minX = Math.min(minX, (n.x ?? 0) - PORT_W);
      minY = Math.min(minY, (n.y ?? 0) - PORT_H);
      maxX = Math.max(maxX, (n.x ?? 0) + w + PORT_W);
      maxY = Math.max(maxY, (n.y ?? 0) + NODE_H + PORT_H);
    }

    const pad = 30;
    const contentW = maxX - minX + pad * 2;
    const contentH = maxY - minY + pad * 2;
    const vw = wrap.clientWidth  || 600;
    const vh = wrap.clientHeight || 400;
    this._scale = Math.min(vw / contentW, vh / contentH, 2);
    this._tx = (vw - contentW * this._scale) / 2 + pad * this._scale - minX * this._scale;
    this._ty = (vh - contentH * this._scale) / 2 + pad * this._scale - minY * this._scale;
    this._updateTransform();
  }
}

customElements.define('pd-patch-graph', PdPatchGraph);
