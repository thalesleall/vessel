// Motion helpers. Motion here answers the user's actions (switching, opening, closing); nothing
// moves on its own. Everything is skipped when the system asks for reduced motion.

const reduceQuery = window.matchMedia("(prefers-reduced-motion: reduce)");
export const reducedMotion = () => reduceQuery.matches;

type ViewTransitionDocument = Document & { startViewTransition?: (update: () => void) => unknown };

/** Applies a DOM-changing update as a View Transition (cross-fade of the changed areas). */
export function transition(update: () => void) {
  const doc = document as ViewTransitionDocument;
  if (!doc.startViewTransition || reducedMotion() || document.hidden) {
    update();
    return;
  }
  try {
    doc.startViewTransition(update);
  } catch {
    update();
  }
}

/**
 * Plays an exit animation for an element that is being removed: a non-interactive copy takes
 * its place for `ms` with `className` applied. Works for components that are unmounted by their
 * parents (modals, the palette, toasts) without changing every caller.
 */
export function exitCopy(el: HTMLElement | undefined, className: string, ms = 160, pin = false) {
  if (!el || reducedMotion()) return;
  const rect = el.getBoundingClientRect();
  if (pin && rect.width === 0) return;
  const copy = el.cloneNode(true) as HTMLElement;
  copy.classList.add(className);
  copy.setAttribute("aria-hidden", "true");
  copy.inert = true;
  copy.removeAttribute("id");
  if (pin) {
    Object.assign(copy.style, {
      position: "fixed",
      top: `${rect.top}px`,
      left: `${rect.left}px`,
      width: `${rect.width}px`,
      margin: "0",
      zIndex: "200",
    });
  }
  document.body.appendChild(copy);
  window.setTimeout(() => copy.remove(), ms);
}

// ---------------------------------------------------------------------------------------------
// Sliding indicators: a pseudo-element on the container follows the active item. Positions are
// passed as CSS variables, so no node is inserted into Solid-managed markup.
// ---------------------------------------------------------------------------------------------

const groups: { container: string; active: string }[] = [
  { container: ".segmented", active: ":scope > button.on" },
  { container: ".auth-tabs", active: ":scope > button.on" },
  { container: ".sidebar", active: ".nav-item.active" },
];

function measure(container: HTMLElement, active: HTMLElement | null): Record<string, string> | null {
  if (!active) return null;
  const c = container.getBoundingClientRect();
  const a = active.getBoundingClientRect();
  return {
    "--slider-x": `${a.left - c.left + container.scrollLeft - container.clientLeft}px`,
    "--slider-y": `${a.top - c.top + container.scrollTop - container.clientTop}px`,
    "--slider-w": `${a.width}px`,
    "--slider-h": `${a.height}px`,
  };
}

function place(container: HTMLElement, vars: Record<string, string> | null) {
  const s = container.style;
  if (!vars) {
    s.setProperty("--slider-o", "0");
    return;
  }
  for (const [k, v] of Object.entries(vars)) s.setProperty(k, v);
  s.setProperty("--slider-o", "1");
  if (!container.classList.contains("has-slider")) {
    container.classList.add("has-slider");
    // Enable the slide only after the first placement, so it does not fly in from 0,0.
    requestAnimationFrame(() => requestAnimationFrame(() => container.classList.add("slider-ready")));
  }
}

let queued = false;
function syncAll() {
  queued = false;
  // Read every position first, then write: interleaving would force a layout per container.
  const reads: [HTMLElement, Record<string, string> | null][] = [];
  for (const g of groups) {
    document.querySelectorAll<HTMLElement>(g.container).forEach((container) => {
      reads.push([container, measure(container, container.querySelector<HTMLElement>(g.active))]);
    });
  }
  for (const [container, vars] of reads) place(container, vars);
}

function schedule() {
  if (queued) return;
  queued = true;
  requestAnimationFrame(syncAll);
}

export function installSliders() {
  new MutationObserver((mutations) => {
    // Terminals change classes constantly (cursor, selection); they never hold a slider.
    if (mutations.some((m) => !(m.target instanceof Element) || !m.target.closest(".term-wrap"))) schedule();
  }).observe(document.body, {
    subtree: true,
    childList: true,
    attributes: true,
    attributeFilter: ["class"],
  });
  window.addEventListener("resize", schedule);
  schedule();
}
