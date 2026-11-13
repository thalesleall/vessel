import { render } from "solid-js/web";
import "@xterm/xterm/css/xterm.css";
import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/600.css";
import "./styles.css";
import "./theme";
import App from "./App";

// `autofocus` only works on page load; honor it for screens and dialogs added later.
new MutationObserver((mutations) => {
  for (const m of mutations) {
    for (const node of m.addedNodes) {
      if (!(node instanceof HTMLElement)) continue;
      const target = node.matches("[autofocus]") ? node : node.querySelector<HTMLElement>("[autofocus]");
      if (target) {
        target.focus();
        return;
      }
    }
  }
}).observe(document.body, { childList: true, subtree: true });

async function start() {
  // Browser preview with a fake backend: `npm run dev`, then open /?mock (dev builds only).
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window) && location.search.includes("mock")) {
    await import("./devmock");
  }
  render(() => <App />, document.getElementById("root")!);
}

start();
