import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { crc32, deflateSync } from "node:zlib";
import { CAPTURE_MAX_DELTA, comparePngs, decodePng } from "./capture-compare.mjs";

const SCRIPT = fileURLToPath(new URL("./capture-compare.mjs", import.meta.url));
const BOARD = fileURLToPath(new URL("../../docs/screenshots/board.png", import.meta.url));

function chunk(type, data) {
  const body = Buffer.concat([Buffer.from(type, "latin1"), data]);
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([length, body, crc]);
}

function paeth(a, b, c) {
  const p = a + b - c;
  const pa = Math.abs(p - a);
  const pb = Math.abs(p - b);
  const pc = Math.abs(p - c);
  return pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
}

// Encodes rows of channel bytes, using filter type (row index % 5) so every filter is exercised.
function encodePng(width, height, channels, pixels) {
  const bpp = channels;
  const stride = width * bpp;
  const out = [];
  for (let y = 0; y < height; y++) {
    const filter = y % 5;
    const row = pixels.subarray(y * stride, (y + 1) * stride);
    const prior = y ? pixels.subarray((y - 1) * stride, y * stride) : new Uint8Array(stride);
    const line = Buffer.alloc(stride + 1);
    line[0] = filter;
    for (let i = 0; i < stride; i++) {
      const a = i >= bpp ? row[i - bpp] : 0;
      const b = prior[i];
      const c = i >= bpp ? prior[i - bpp] : 0;
      const predictor = [0, a, b, (a + b) >> 1, paeth(a, b, c)][filter];
      line[i + 1] = (row[i] - predictor) & 0xff;
    }
    out.push(line);
  }
  const header = Buffer.alloc(13);
  header.writeUInt32BE(width, 0);
  header.writeUInt32BE(height, 4);
  header.set([8, channels === 4 ? 6 : 2, 0, 0, 0], 8);
  return Buffer.concat([
    Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]),
    chunk("IHDR", header),
    chunk("IDAT", deflateSync(Buffer.concat(out))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

function pattern(width, height, channels) {
  const pixels = new Uint8Array(width * height * channels);
  for (let i = 0; i < pixels.length; i++) pixels[i] = (i * 37 + 11) % 256;
  return pixels;
}

function shifted(pixels, index, delta) {
  const copy = Uint8Array.from(pixels);
  copy[index] += delta;
  return copy;
}

test("decodes every filter type for RGB and RGBA", () => {
  for (const channels of [3, 4]) {
    const pixels = pattern(7, 10, channels);
    const { width, height, rgba } = decodePng(encodePng(7, 10, channels, pixels));
    assert.deepEqual([width, height], [7, 10]);
    for (let p = 0; p < 70; p++) {
      for (let c = 0; c < channels; c++) assert.equal(rgba[p * 4 + c], pixels[p * channels + c]);
      if (channels === 3) assert.equal(rgba[p * 4 + 3], 255);
    }
  }
});

test("passes at or under the max delta and fails above it", () => {
  const pixels = pattern(6, 5, 4);
  pixels[9] = 0;
  const base = encodePng(6, 5, 4, pixels);
  for (const [delta, ok] of [
    [0, true],
    [10, true],
    [16, true],
    [17, false],
    [203, false],
  ]) {
    const result = comparePngs(base, encodePng(6, 5, 4, shifted(pixels, 9, delta)));
    assert.deepEqual(
      result,
      { ok, sizes: ["6x5", "6x5"], differing: delta ? 1 : 0, worstDelta: delta },
      `${delta}`,
    );
  }
  assert.equal(CAPTURE_MAX_DELTA, 16);
});

test("fails on a size mismatch", () => {
  const result = comparePngs(
    encodePng(2, 2, 3, pattern(2, 2, 3)),
    encodePng(3, 2, 3, pattern(3, 2, 3)),
  );
  assert.deepEqual(result, { ok: false, sizes: ["2x2", "3x2"], differing: null, worstDelta: null });
});

test("rejects an unsupported PNG", () => {
  const png = encodePng(1, 1, 3, pattern(1, 1, 3));
  png[24] = 16;
  assert.throws(() => decodePng(png), /^Error: unsupported PNG: /);
});

test("board.png matches itself", () => {
  const board = readFileSync(BOARD);
  assert.deepEqual(comparePngs(board, board), {
    ok: true,
    sizes: ["2560x1600", "2560x1600"],
    differing: 0,
    worstDelta: 0,
  });
});

test("CLI prints one JSON line and exits by result", () => {
  const dir = mkdtempSync(join(tmpdir(), "capture-compare-"));
  const pixels = pattern(4, 4, 3);
  pixels[0] = 0;
  const a = join(dir, "a.png");
  const b = join(dir, "b.png");
  writeFileSync(a, encodePng(4, 4, 3, pixels));
  writeFileSync(b, encodePng(4, 4, 3, shifted(pixels, 0, 17)));
  const failed = spawnSync(process.execPath, [SCRIPT, a, b], { encoding: "utf8" });
  assert.equal(failed.status, 1);
  assert.deepEqual(JSON.parse(failed.stdout), {
    a,
    b,
    maxDelta: 16,
    ok: false,
    sizes: ["4x4", "4x4"],
    differing: 1,
    worstDelta: 17,
  });
  const passed = spawnSync(process.execPath, [SCRIPT, a, b, "--max-delta", "17"], {
    encoding: "utf8",
  });
  assert.equal(passed.status, 0);
  assert.equal(JSON.parse(passed.stdout).maxDelta, 17);
  for (const args of [[a], [a, b, "--max-delta", "256"], [a, join(dir, "missing.png")]]) {
    const usage = spawnSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8" });
    assert.equal(usage.status, 2);
    assert.equal(usage.stdout, "");
    assert.match(usage.stderr, /^capture compare: /);
  }
});
