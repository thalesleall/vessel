import { render } from "solid-js/web";
import "@xterm/xterm/css/xterm.css";
import "@fontsource/ibm-plex-sans/400.css";
import "@fontsource/ibm-plex-sans/500.css";
import "@fontsource/ibm-plex-sans/600.css";
import "@fontsource/ibm-plex-mono/400.css";
import "@fontsource/ibm-plex-mono/600.css";
import "./styles.css";
import "./theme";
import { installSliders } from "./motion";
import App from "./App";
import { errorMessage } from "./api";
import { toast } from "./store";

// An action that fails outside a handled path must never look like "nothing happened".
window.addEventListener("unhandledrejection", (e) => {
  console.error(e.reason);
  toast(`Algo deu errado: ${errorMessage(e.reason)}`, "error");
});
window.addEventListener("error", (e) => {
  if (!e.error) return; // resource loads and ResizeObserver notices carry no error object
  console.error(e.error);
  toast(`Algo deu errado: ${errorMessage(e.error)}`, "error");
});

// For screens and dialogs added after load: honor `autofocus` (the browser only does it on page
// load), and keep autofill suggestions off fields that are not sign-in fields.
new MutationObserver((mutations) => {
  let focused = false;
  for (const m of mutations) {
    for (const node of m.addedNodes) {
      if (!(node instanceof HTMLElement)) continue;
      const fields = node.matches("input, textarea") ? [node] : [...node.querySelectorAll<HTMLElement>("input, textarea")];
      for (const f of fields) if (!f.hasAttribute("autocomplete")) f.setAttribute("autocomplete", "off");
      if (focused) continue;
      const target = node.matches("[autofocus]") ? node : node.querySelector<HTMLElement>("[autofocus]");
      if (target) {
        target.focus();
        focused = true;
      }
    }
  }
}).observe(document.body, { childList: true, subtree: true });

installSliders();

async function start() {
  // Browser preview with a fake backend: `npm run dev`, then open /?mock (dev builds only).
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window) && location.search.includes("mock")) {
    await import("./devmock");
  }
  render(() => <App />, document.getElementById("root")!);
}

start();
