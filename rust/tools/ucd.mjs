// The Unicode Character Database files the generators read, so their output depends on the UCD
// version passed in and not on the Node in use. The committed tables are from Unicode 12.1.0:
// https://www.unicode.org/Public/12.1.0/ucd/UnicodeData.txt and CompositionExclusions.txt.
import { readFileSync } from "node:fs";
import { join } from "node:path";

export const MAX = 0x10ffff;

export function loadUcd(dir) {
  const category = new Map();
  const ccc = new Map();
  const decomposition = new Map();
  let first = null;
  for (const line of readFileSync(join(dir, "UnicodeData.txt"), "utf8").split("\n")) {
    if (!line) continue;
    const f = line.split(";");
    const cp = parseInt(f[0], 16);
    if (f[1].endsWith(", First>")) {
      first = cp;
      continue;
    }
    const from = f[1].endsWith(", Last>") ? first : cp;
    for (let c = from; c <= cp; c++) {
      category.set(c, f[2]);
      if (f[3] !== "0") ccc.set(c, Number(f[3]));
    }
    if (f[5]) {
      const parts = f[5].split(" ");
      const compat = parts[0].startsWith("<");
      decomposition.set(cp, {
        compat,
        chars: (compat ? parts.slice(1) : parts).map((h) => parseInt(h, 16)),
      });
    }
  }
  const excluded = new Set();
  for (const line of readFileSync(join(dir, "CompositionExclusions.txt"), "utf8").split("\n")) {
    const code = line.split("#")[0].trim();
    if (code) excluded.add(parseInt(code, 16));
  }
  return {
    category: (cp) => category.get(cp) ?? "Cn",
    ccc: (cp) => ccc.get(cp) ?? 0,
    decomposition,
    excluded,
  };
}

// Python's str.isprintable for one code point.
export function printable(ucd, cp) {
  if (cp === 0x20) return true;
  return !["Cc", "Cf", "Cs", "Co", "Cn", "Zl", "Zp", "Zs"].includes(ucd.category(cp));
}

const S_BASE = 0xac00;
const L_BASE = 0x1100;
const V_BASE = 0x1161;
const T_BASE = 0x11a7;
const T_COUNT = 28;
const N_COUNT = 588;
const S_COUNT = 11172;

// NFKC of a code point sequence: full compatibility decomposition, canonical order, then
// canonical composition, as UAX #15 defines them.
export function nfkc(ucd, cps) {
  const decomposed = [];
  const decompose = (cp) => {
    const s = cp - S_BASE;
    if (s >= 0 && s < S_COUNT) {
      decomposed.push(
        L_BASE + Math.floor(s / N_COUNT),
        V_BASE + Math.floor((s % N_COUNT) / T_COUNT),
      );
      if (s % T_COUNT) decomposed.push(T_BASE + (s % T_COUNT));
      return;
    }
    const d = ucd.decomposition.get(cp);
    if (d) d.chars.forEach(decompose);
    else decomposed.push(cp);
  };
  cps.forEach(decompose);
  for (let i = 1; i < decomposed.length; i++) {
    for (let j = i; j > 0; j--) {
      const a = ucd.ccc(decomposed[j - 1]);
      const b = ucd.ccc(decomposed[j]);
      if (b === 0 || a <= b) break;
      [decomposed[j - 1], decomposed[j]] = [decomposed[j], decomposed[j - 1]];
    }
  }
  return compose(ucd, decomposed);
}

let primaries;
function primary(ucd, a, b) {
  if (!primaries) {
    primaries = new Map();
    for (const [cp, d] of ucd.decomposition) {
      if (d.compat || d.chars.length !== 2 || ucd.excluded.has(cp)) continue;
      if (ucd.ccc(cp) !== 0 || ucd.ccc(d.chars[0]) !== 0) continue;
      primaries.set(`${d.chars[0]},${d.chars[1]}`, cp);
    }
  }
  const l = a - L_BASE;
  const v = b - V_BASE;
  if (l >= 0 && l < 19 && v >= 0 && v < 21) return S_BASE + (l * 21 + v) * T_COUNT;
  const s = a - S_BASE;
  const t = b - T_BASE;
  if (s >= 0 && s < S_COUNT && s % T_COUNT === 0 && t > 0 && t < T_COUNT) return a + t;
  return primaries.get(`${a},${b}`);
}

function compose(ucd, cps) {
  if (!cps.length) return cps;
  const out = [cps[0]];
  let starter = ucd.ccc(cps[0]) === 0 ? 0 : -1;
  let last = ucd.ccc(cps[0]);
  for (const cp of cps.slice(1)) {
    const c = ucd.ccc(cp);
    const blocked = out.length - 1 !== starter && (last === 0 || last >= c);
    const composite = starter >= 0 && !blocked ? primary(ucd, out[starter], cp) : undefined;
    if (composite !== undefined) {
      out[starter] = composite;
      continue;
    }
    if (c === 0) starter = out.length;
    last = c;
    out.push(cp);
  }
  return out;
}
