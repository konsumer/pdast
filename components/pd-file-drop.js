/**
 * <pd-file-drop>
 *
 * Reusable drag-and-drop / click-to-browse file input zone.
 *
 * Attributes:
 *   accept   — comma-separated MIME types or extensions (default ".pd,.json")
 *   multiple — allow multiple files (boolean attribute)
 *   label    — prompt text shown in the zone
 *
 * Events:
 *   pd-files  — fired when files are selected; detail = FileList
 *
 * Slots:
 *   (default) — replaces the default prompt text
 *
 * CSS custom properties (inherited from :host):
 *   see pd-styles.js
 */

import { baseCSS } from './pd-styles.js';

const css = `
  :host {
    display: block;
  }
  .zone {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.5em;
    padding: 2em 1.5em;
    border: 2px dashed var(--pd-border);
    border-radius: var(--pd-radius);
    background: var(--pd-surface);
    cursor: pointer;
    transition: border-color 0.15s, background 0.15s;
    text-align: center;
    min-height: 100px;
    user-select: none;
  }
  .zone.over, .zone:hover {
    border-color: var(--pd-accent);
    background: var(--pd-obj);
  }
  .zone.over { background: color-mix(in srgb, var(--pd-accent) 12%, var(--pd-obj)); }
  .icon { font-size: 2em; line-height: 1; }
  .label { color: var(--pd-text-dim); font-size: 0.9em; }
  .label strong { color: var(--pd-accent); }
  input[type=file] { display: none; }
`;

class PdFileDrop extends HTMLElement {
  static observedAttributes = ['accept', 'multiple', 'label'];

  constructor() {
    super();
    this.attachShadow({ mode: 'open' });
    const sheet = new CSSStyleSheet();
    sheet.replaceSync(baseCSS + css);
    this.shadowRoot.adoptedStyleSheets = [sheet];
    this._render();
    this._bind();
  }

  attributeChangedCallback() { this._render(); this._bind(); }

  get accept()   { return this.getAttribute('accept')   ?? '.pd,.json'; }
  get multiple() { return this.hasAttribute('multiple'); }
  get label()    { return this.getAttribute('label')    ?? 'Drop files here or click to browse'; }

  _render() {
    this.shadowRoot.innerHTML = `
      <div class="zone" part="zone" role="button" tabindex="0"
           aria-label="${this.label}">
        <span class="icon" aria-hidden="true">📂</span>
        <span class="label">
          <slot><strong>Drop files</strong> here or click to browse</slot>
        </span>
      </div>
      <input type="file"
             accept="${this.accept}"
             ${this.multiple ? 'multiple' : ''}>
    `;
  }

  _bind() {
    const zone  = this.shadowRoot.querySelector('.zone');
    const input = this.shadowRoot.querySelector('input');
    if (!zone || !input) return;

    zone.onclick    = () => input.click();
    zone.onkeydown  = e => (e.key === 'Enter' || e.key === ' ') && input.click();
    input.onchange  = () => this._emit(input.files);

    zone.ondragover  = e => { e.preventDefault(); zone.classList.add('over'); };
    zone.ondragleave = ()=> zone.classList.remove('over');
    zone.ondrop      = e => {
      e.preventDefault();
      zone.classList.remove('over');
      this._emit(e.dataTransfer.files);
    };
  }

  _emit(files) {
    if (!files?.length) return;
    this.dispatchEvent(new CustomEvent('pd-files', {
      detail: files, bubbles: true, composed: true
    }));
  }
}

customElements.define('pd-file-drop', PdFileDrop);
