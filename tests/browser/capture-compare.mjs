// Compares two captures of one side and tolerates Chrome anti-aliasing noise: a pair passes
// when the sizes match and no channel differs by more than the max delta.
// Usage: node tests/browser/capture-compare.mjs <a.png> <b.png> [--max-delta N]
import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { inflateSync } from "node:zlib";

export const CAPTURE_MAX_DELTA = 16;

const SIGNATURE = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);

function paeth(a, b, c) {
  const p = a + b - c;
  const pa = Math.abs(p - a);
  const pb = Math.abs(p - b);
  const pc = Math.abs(p - c);
  return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
}

export function decodePng(buffer) {
  if (!buffer.subarray(0, 8).equals(SIGNATURE)) throw new Error("unsupported PNG: bad signature");
  let header;
  const idat = [];
  for (let at = 8; at + 8 <= buffer.length;) {
    const length = buffer.readUInt32BE(at);
    const type = buffer.toString("latin1", at + 4, at + 8);
    const data = buffer.subarray(at + 8, at + 8 + length);
    if (type === "IHDR") header = data;
    else if (type === "IDAT") idat.push(data);
    else if (type === "IEND") break;
    at += 12 + length;
  }
  if (!header) throw new Error("unsupported PNG: no IHDR");
  const width = header.readUInt32BE(0);
  const height = header.readUInt32BE(4);
  const [depth, colorType, , , interlace] = header.subarray(8, 13);
  if (depth !== 8) throw new Error(`unsupported PNG: bit depth ${depth}`);
  if (colorType !== 2 && colorType !== 6)
    throw new Error(`unsupported PNG: color type ${colorType}`);
  if (interlace !== 0) throw new Error("unsupported PNG: interlaced");
  const bpp = colorType === 6 ? 4 : 3;
  const stride = width * bpp;
  const raw = inflateSync(Buffer.concat(idat));
  if (raw.length < (stride + 1) * height) throw new Error("unsupported PNG: short image data");
  const pixels = new Uint8Array(stride * height);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    if (filter > 4) throw new Error(`unsupported PNG: filter ${filter}`);
    const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    const row = y * stride;
    for (let i = 0; i < stride; i++) {
      const a = i >= bpp ? pixels[row + i - bpp] : 0;
      const b = y ? pixels[row - stride + i] : 0;
      let predictor = 0;
      switch (filter) {
        case 1:
          predictor = a;
          break;
        case 2:
          predictor = b;
          break;
        case 3:
          predictor = (a + b) >> 1;
          break;
        case 4:
          predictor = paeth(a, b, y && i >= bpp ? pixels[row - stride + i - bpp] : 0);
      }
      pixels[row + i] = (line[i] + predictor) & 0xff;
    }
  }
  if (bpp === 4) return { width, height, rgba: pixels };
  const rgba = new Uint8Array(width * height * 4);
  for (let p = 0, q = 0; p < pixels.length; p += 3, q += 4) {
    rgba[q] = pixels[p];
    rgba[q + 1] = pixels[p + 1];
    rgba[q + 2] = pixels[p + 2];
    rgba[q + 3] = 255;
  }
  return { width, height, rgba };
}

export function comparePngs(a, b, maxDelta = CAPTURE_MAX_DELTA) {
  const left = decodePng(a);
  const right = decodePng(b);
  const sizes = [`${left.width}x${left.height}`, `${right.width}x${right.height}`];
  if (sizes[0] !== sizes[1]) return { ok: false, sizes, differing: null, worstDelta: null };
  let differing = 0;
  let worstDelta = 0;
  for (let p = 0; p < left.rgba.length; p += 4) {
    let pixel = 0;
    for (let c = p; c < p + 4; c++) pixel = Math.max(pixel, Math.abs(left.rgba[c] - right.rgba[c]));
    if (pixel) differing++;
    worstDelta = Math.max(worstDelta, pixel);
  }
  return { ok: worstDelta <= maxDelta, sizes, differing, worstDelta };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const args = process.argv.slice(2);
    const usage =
      "usage: capture-compare.mjs <a.png> <b.png> [--max-delta N], with N from 0 to 255";
    if (args.length !== 2 && (args.length !== 4 || args[2] !== "--max-delta"))
      throw new Error(usage);
    if (args.length === 4 && !(/^\d{1,3}$/.test(args[3]) && Number(args[3]) <= 255))
      throw new Error(usage);
    const maxDelta = args.length === 4 ? Number(args[3]) : CAPTURE_MAX_DELTA;
    const [a, b] = args;
    const result = comparePngs(readFileSync(a), readFileSync(b), maxDelta);
    console.log(JSON.stringify({ a, b, maxDelta, ...result }));
    process.exitCode = result.ok ? 0 : 1;
  } catch (error) {
    console.error(`capture compare: ${error.message}`);
    process.exitCode = 2;
  }
}
