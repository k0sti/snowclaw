/**
 * Snow UI entry point.
 *
 * Initializes navigation and the debug console.
 */

import { init as initDebugConsole } from './debug-console';

// TODO: Import WASM bindings once build pipeline is configured
// import init as initWasm from '../pkg/snow_ui';

/** Simple panel navigation via sidebar links. */
function initNav(): void {
  const links = document.querySelectorAll<HTMLAnchorElement>('.nav-link');
  const panels = document.querySelectorAll<HTMLElement>('.panel');

  links.forEach((link) => {
    link.addEventListener('click', (e) => {
      e.preventDefault();
      const target = link.dataset.panel;
      if (!target) return;

      links.forEach((l) => l.classList.remove('active'));
      link.classList.add('active');

      panels.forEach((p) => {
        p.classList.toggle('active', p.id === `panel-${target}`);
      });
    });
  });
}

async function main(): Promise<void> {
  // TODO: Initialize WASM module
  // await initWasm();

  initNav();
  initDebugConsole();
}

main();
