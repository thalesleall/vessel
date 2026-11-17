// Form validation feedback. A form never refuses an action silently: the reason shows as a
// message and, when it belongs to a field, next to that field, which gets focus.
import { reducedMotion } from "./motion";
import { toast } from "./store";

export const BLANK = "Não pode ficar em branco.";

/** Shows why an action was refused. `target` is the start of the field's label (or the control
 * itself), or null when the problem does not belong to one field. Returns false, so callers can
 * `return invalid(...)`. */
export function invalid(target: string | HTMLElement | null, message: string): false {
  toast(message, "error");
  if (!target) return false;
  let field: HTMLElement | null | undefined;
  let control: HTMLElement | null | undefined;
  if (typeof target === "string") {
    const scope: ParentNode = [...document.querySelectorAll(".modal-backdrop:not(.leaving) .modal")].pop() ?? document;
    field = [...scope.querySelectorAll<HTMLElement>(".field")]
      .find((f) => f.querySelector(".field-label")?.textContent?.trim().startsWith(target));
    control = field?.querySelector<HTMLElement>("input:not([type=checkbox]), textarea, select")
      ?? field?.querySelector<HTMLElement>("input, button");
  } else {
    control = target;
    field = target.closest<HTMLElement>(".field, [data-field]") ?? target.parentElement;
  }
  if (!field) return false;
  // A checkbox's label is a row: its message goes on its own line below it.
  const row = !field.matches(".field, [data-field]");
  const old = row ? field.nextElementSibling : field.querySelector(".field-error");
  if (old?.matches(".field-error")) old.remove();
  const note = document.createElement("span");
  note.className = "field-error";
  note.setAttribute("role", "alert");
  note.textContent = message;
  if (row) {
    note.style.display = "block";
    note.style.margin = "0 0 10px";
    field.after(note);
  } else if (control && control.parentElement === field) control.after(note);
  else field.append(note);
  const clear = () => {
    if (!note.isConnected) return; // a newer message replaced this one
    control?.removeAttribute("aria-invalid");
    note.remove();
  };
  if (control) {
    control.setAttribute("aria-invalid", "true");
    control.focus({ preventScroll: true });
    control.addEventListener("input", clear, { once: true });
    control.addEventListener("change", clear, { once: true });
  } else {
    field.addEventListener("change", clear, { once: true });
  }
  (control ?? field).scrollIntoView({ block: "center", behavior: reducedMotion() ? "auto" : "smooth" });
  return false;
}

/** A whole number from `min` to `max`, or null. */
export function intIn(value: unknown, min: number, max: number): number | null {
  const n = typeof value === "string" ? (value.trim() === "" ? NaN : Number(value)) : Number(value);
  return Number.isInteger(n) && n >= min && n <= max ? n : null;
}

export const PORT_MSG = "Porta inválida: use um número de 1 a 65535.";
export const isPort = (v: unknown) => intIn(v, 1, 65535) !== null;
export const looksLikeEmail = (s: string) => /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(s.trim());
export const looksLikeUrl = (s: string) => /^https?:\/\/\S+$/i.test(s.trim());
