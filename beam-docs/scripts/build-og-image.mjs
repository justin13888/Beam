/**
 * Renders public/og/beam.png, the Open Graph card.
 *
 * Committed as a PNG rather than generated during `astro build` because social crawlers need a
 * stable, unhashed URL that Starlight's static `head` descriptors can name. Re-run with
 * `bun run og:build` after editing the artwork.
 */
import { mkdir, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import sharp from "sharp";

const here = dirname(fileURLToPath(import.meta.url));
const out = resolve(here, "../public/og/beam.png");

const BG = "#0b1220";
const ACCENT = "#22d3ee";
const font =
	"ui-sans-serif, system-ui, -apple-system, 'Segoe UI', Roboto, 'Helvetica Neue', sans-serif";

const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630" viewBox="0 0 1200 630">
  <rect width="1200" height="630" fill="${BG}"/>
  <g transform="translate(96 232) scale(3.4)" fill="none" stroke="${ACCENT}" stroke-linecap="round" stroke-width="2.75">
    <path d="M8 16h5"/>
    <path d="M8 16l12-7" opacity=".72"/>
    <path d="M8 16l12 7" opacity=".72"/>
    <path d="M23.5 7.5v17" opacity=".38"/>
  </g>
  <circle cx="${96 + 8 * 3.4}" cy="${232 + 16 * 3.4}" r="${3 * 3.4}" fill="${ACCENT}"/>
  <text x="232" y="300" fill="#ffffff" font-family="${font}" font-size="92" font-weight="700" letter-spacing="-2">Beam</text>
  <text x="96" y="404" fill="#cbd5e1" font-family="${font}" font-size="38" font-weight="500">Self-hosted media, streamed without transcoding.</text>
  <text x="96" y="470" fill="#64748b" font-family="${font}" font-size="28" font-weight="500">beam.justinchung.net</text>
  <rect x="96" y="520" width="88" height="6" rx="3" fill="${ACCENT}"/>
</svg>`;

await mkdir(dirname(out), { recursive: true });
await writeFile(out, await sharp(Buffer.from(svg)).png({ compressionLevel: 9 }).toBuffer());
console.log(`wrote ${out}`);
