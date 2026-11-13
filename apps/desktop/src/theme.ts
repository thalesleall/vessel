import { createSignal } from "solid-js";

/** Interface theme. A per-computer preference, so it lives in localStorage, not in the vault. */
export type ThemePref = "system" | "dark" | "light";

const KEY = "vessel.theme";
const media = window.matchMedia("(prefers-color-scheme: light)");

function read(): ThemePref {
  try {
    const v = localStorage.getItem(KEY);
    return v === "dark" || v === "light" ? v : "system";
  } catch {
    return "system";
  }
}

const [pref, setPrefSignal] = createSignal<ThemePref>(read());
const [resolved, setResolved] = createSignal<"dark" | "light">("dark");

function apply() {
  const p = pref();
  const theme = p === "system" ? (media.matches ? "light" : "dark") : p;
  setResolved(theme);
  document.documentElement.dataset.theme = theme;
}

export const themePref = pref;
export const resolvedTheme = resolved;

export function setThemePref(p: ThemePref) {
  try {
    localStorage.setItem(KEY, p);
  } catch {
    /* preference is only remembered when storage works */
  }
  setPrefSignal(p);
  apply();
}

media.addEventListener("change", apply);
apply();
