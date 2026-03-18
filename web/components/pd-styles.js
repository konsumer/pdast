/**
 * Shared CSS custom properties and base styles injected into every
 * pdast web component's shadow root.
 *
 * Components import this and spread it into their adoptedStyleSheets:
 *   const sheet = new CSSStyleSheet();
 *   sheet.replaceSync(baseCSS + componentCSS);
 */

export const baseCSS = `
  :host {
    --pd-bg:          #1e1e2e;
    --pd-surface:     #2a2a3e;
    --pd-border:      #44445a;
    --pd-text:        #cdd6f4;
    --pd-text-dim:    #7f849c;
    --pd-accent:      #89b4fa;
    --pd-accent2:     #a6e3a1;
    --pd-warn:        #f9e2af;
    --pd-error:       #f38ba8;
    --pd-obj:         #313244;
    --pd-obj-border:  #585b70;
    --pd-wire:        #89b4fa;
    --pd-wire-ctrl:   #a6e3a1;
    --pd-radius:      6px;
    --pd-font:        'JetBrains Mono', 'Fira Mono', monospace;
    --pd-font-ui:     system-ui, sans-serif;

    font-family: var(--pd-font-ui);
    color: var(--pd-text);
    box-sizing: border-box;
  }
  *, *::before, *::after { box-sizing: inherit; }

  button {
    cursor: pointer;
    border: 1px solid var(--pd-border);
    border-radius: var(--pd-radius);
    background: var(--pd-surface);
    color: var(--pd-text);
    padding: 0.3em 0.8em;
    font: inherit;
    transition: border-color 0.15s, background 0.15s;
  }
  button:hover { border-color: var(--pd-accent); background: var(--pd-obj); }
  button:active { background: var(--pd-bg); }

  code, pre {
    font-family: var(--pd-font);
    font-size: 0.85em;
  }
`
