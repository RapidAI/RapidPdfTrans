// Rasterize one PDF page with pdf.js. The glyph-coverage test shells out to this.
import { createRequire } from "node:module";
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

function viewersDir() {
  const candidates = [
    process.env.RPT_VIEWERS_DIR,
    "/tmp/rpt-viewers",
    "/tmp/pdfjs",
  ].filter(Boolean);
  for (const dir of candidates) {
    if (existsSync(`${dir}/node_modules/pdfjs-dist/package.json`)) {
      return dir;
    }
  }
  throw new Error("pdfjs-dist is not installed under RPT_VIEWERS_DIR, /tmp/rpt-viewers, or /tmp/pdfjs");
}

const require = createRequire(pathToFileURL(`${viewersDir()}/package.json`));
const { createCanvas } = require("@napi-rs/canvas");
const pdfjsPath = require.resolve("pdfjs-dist/legacy/build/pdf.mjs");
const { getDocument } = await import(pathToFileURL(pdfjsPath).href);

if (process.argv[2] === "--check") {
  process.exit(0);
}

const pdfPath = process.argv[2];
const outPath = process.argv[3];
const pageNumber = Number(process.argv[4] || "1");
const data = new Uint8Array(readFileSync(pdfPath));
const doc = await getDocument({ data, disableWorker: true, useSystemFonts: false }).promise;
const page = await doc.getPage(pageNumber);
const viewport = page.getViewport({ scale: 2 });
const canvas = createCanvas(viewport.width, viewport.height);
const ctx = canvas.getContext("2d");
ctx.fillStyle = "white";
ctx.fillRect(0, 0, canvas.width, canvas.height);
await page.render({ canvasContext: ctx, viewport }).promise;
writeFileSync(outPath, canvas.toBuffer("image/png"));
