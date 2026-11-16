// Regenerates every app icon from the logo sources:
//
//   node scripts/build-icons.mjs
//
// apps/desktop/logo.svg is the mark on a 32 px grid (every edge on a whole pixel at 32 px) and is
// used for 32 px and for the large sizes. At 16, 24 or 48 px its edges would fall between pixels
// and blur, so those sizes come from apps/desktop/icon-src/logo-<size>.svg, the same mark redrawn
// on their own pixel grids, and replace the scaled frames inside icon.ico.
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const desktop = new URL("../apps/desktop/", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");
const icons = join(desktop, "src-tauri", "icons");
const cli = join(desktop, "node_modules", "@tauri-apps", "cli", "tauri.js");

function tauriIcon(source, output) {
  execFileSync(process.execPath, [cli, "icon", source, "-o", output], { cwd: desktop, stdio: "ignore" });
}

/** The PNG frames of an .ico, by width. */
function readIco(file) {
  const b = readFileSync(file);
  const frames = new Map();
  for (let i = 0; i < b.readUInt16LE(4); i++) {
    const e = 6 + 16 * i;
    const size = b.readUInt32LE(e + 8);
    const offset = b.readUInt32LE(e + 12);
    frames.set(b[e] || 256, b.subarray(offset, offset + size));
  }
  return frames;
}

/** An .ico holding the given PNG frames (width -> PNG bytes). */
function writeIco(file, frames) {
  const sizes = [...frames.keys()].sort((a, b) => a - b);
  const header = Buffer.alloc(6 + 16 * sizes.length);
  header.writeUInt16LE(1, 2);
  header.writeUInt16LE(sizes.length, 4);
  let offset = header.length;
  sizes.forEach((s, i) => {
    const e = 6 + 16 * i;
    const png = frames.get(s);
    header[e] = s >= 256 ? 0 : s;
    header[e + 1] = s >= 256 ? 0 : s;
    header.writeUInt16LE(1, e + 4);
    header.writeUInt16LE(32, e + 6);
    header.writeUInt32LE(png.length, e + 8);
    header.writeUInt32LE(offset, e + 12);
    offset += png.length;
  });
  writeFileSync(file, Buffer.concat([header, ...sizes.map((s) => frames.get(s))]));
}

// 1. Every file from the 32 px master.
tauriIcon("logo.svg", icons);
for (const mobile of ["android", "ios"]) rmSync(join(icons, mobile), { recursive: true, force: true });

// 2. The 16, 24 and 48 px frames from their own drawings.
const ico = readIco(join(icons, "icon.ico"));
for (const size of [16, 24, 48]) {
  const tmp = mkdtempSync(join(tmpdir(), "vessel-icon-"));
  try {
    tauriIcon(join("icon-src", `logo-${size}.svg`), tmp);
    ico.set(size, readIco(join(tmp, "icon.ico")).get(size));
  } finally {
    rmSync(tmp, { recursive: true, force: true });
  }
}
writeIco(join(icons, "icon.ico"), ico);
console.log(`icons regenerated; icon.ico frames: ${[...ico.keys()].sort((a, b) => a - b).join(", ")}`);
