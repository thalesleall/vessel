import { render } from "solid-js/web";
import "@xterm/xterm/css/xterm.css";
import "./styles.css";
import App from "./App";

async function start() {
  // Browser preview with a fake backend: `npm run dev`, then open /?mock (dev builds only).
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window) && location.search.includes("mock")) {
    await import("./devmock");
  }
  render(() => <App />, document.getElementById("root")!);
}

start();
