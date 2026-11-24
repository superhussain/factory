// Board storage. The board's rows live in the BoardEvents room's SQLite, packed in order into
// pages of about PAGE_BYTES, with one index row per board row for the lookups a PATCH needs. A
// row PATCH reads and rewrites only the pages its rows sit on, and a whole-board read costs one
// SQL row per page. The Workers Free plan caps SQLite at 5M rows read and 100k rows written a
// day, so every operation's row count is budgeted in build.md and pinned in a test.
//
// GET /board.json streams R2's bytes while R2's copy is the current version, and otherwise is
// built from the pages and cached per version in memory, which an eviction loses. R2's
// board.json is a copy the room writes: before a full write answers, and within FLUSH_MS of a
// row commit. Each copy is tagged with the room's sequence number, so the room knows its own
// copy after a restart. The room adopts a board.json that something else wrote to R2, such as a
// Worker version from before this store or an operator's restore, and merges its own unflushed
// rows over it.
//
// boardStore(env) is the Worker's handle. Without a room binding (unit tests that give the Worker
// a memory bucket alone, or a room stub with fetch alone) it reads and writes R2's board.json
// directly, the whole object each time, as the Worker did before this store.

export const BOARD_KEY = "board.json";
export const ROOM = "main";
export const FLUSH_MS = 30_000;
export const PAGE_BYTES = 256 << 10;
const MAX_ROW = 1 << 20;
// Characters per board_meta value: at most 3 UTF-8 bytes each, under SQLite's 2 MB limit.
const META_CHUNK = 512 << 10;
// The customMetadata store value on the room's own copies in R2.
const ROOM_COPY = "room";

const ID_PREFIX = /^(\d{3,})-/;
const encoder = new TextEncoder();
const byteLength = (text) => encoder.encode(text).length;
const isDoc = (doc) => typeof doc === "object" && doc !== null && !Array.isArray(doc);
const newEtag = () => `"b${crypto.randomUUID().replaceAll("-", "")}"`;
const rowId = (row) => (typeof row?.id === "string" ? row.id : null);

function parseDoc(text) {
  try {
    const doc = JSON.parse(text);
    return isDoc(doc) ? doc : null;
  } catch {
    return null;
  }
}

// A short fingerprint of a row's JSON, or null for no row: two FNV-1a hashes and the length.
// A merge compares R2's row with the one the room last copied by this alone.
function rowHash(json) {
  if (json === null) return null;
  let a = 0x811c9dc5,
    b = 0x050c5d1f;
  for (let i = 0; i < json.length; i++) {
    const c = json.charCodeAt(i);
    a = Math.imul(a ^ c, 0x01000193);
    b = Math.imul(b ^ c, 0x5bd1e995);
  }
  const hex = (n) => (n >>> 0).toString(16).padStart(8, "0");
  return hex(a) + hex(b) + json.length.toString(16);
}

// The idea upload names a row carries, the ones checkIdeaImages and checkIdeaFiles subtract.
export function rowNames(row) {
  const names = [];
  for (const field of ["idea_images", "idea_files"])
    if (Array.isArray(row?.[field]))
      for (const n of row[field]) if (typeof n === "string") names.push(n);
  return names;
}

// Every upload name the stored rows carry. An unreadable row vouches for nothing past it.
export function storedNames(jobs) {
  const names = new Set();
  try {
    for (const j of jobs) {
      for (const n of j.idea_images ?? []) names.add(n);
      for (const n of j.idea_files ?? []) names.add(n);
    }
  } catch {}
  return names;
}

// The index columns of one board row.
function rowColumns(row) {
  const id = rowId(row);
  const m = id !== null && ID_PREFIX.exec(id);
  const names = rowNames(row);
  return {
    id,
    stage: id !== null ? JSON.stringify(row.stage ?? null) : null,
    num: m ? Number(m[1]) : null,
    capture: typeof row?.capture_id === "string" ? row.capture_id : null,
    names: names.length ? JSON.stringify(names) : null,
  };
}

const sameColumns = (a, b) => a.num === b.num && a.capture === b.capture && a.names === b.names;

// A document's top-level keys in order, each with its value's JSON. An array jobs value is
// null here: the pages hold it.
function headPairs(doc) {
  return Object.keys(doc).map((k) => [
    k,
    k === "jobs" && Array.isArray(doc.jobs) ? null : JSON.stringify(doc[k]),
  ]);
}

// Sets one top-level key in place, or appends it, as { ...doc, [key]: value } does.
function setPair(pairs, key, json) {
  const out = pairs.map((p) => (p[0] === key ? [key, json] : p));
  if (!out.some((p) => p[0] === key)) out.push([key, json]);
  return out;
}

const headObject = (pairs) =>
  Object.fromEntries(pairs.filter(([k]) => k !== "jobs").map(([k, v]) => [k, JSON.parse(v)]));
// The head as compared across a merge: every key but updated, which takes the later stamp.
const headKey = (pairs) => JSON.stringify(pairs.filter(([k]) => k !== "updated"));
const pairValue = (pairs, key) => {
  const pair = pairs.find(([k]) => k === key);
  return pair ? JSON.parse(pair[1]) : undefined;
};

// The board document's text: JSON.stringify of the document the head and rows make. rows is
// the comma-joined JSON of every row.
function boardText(pairs, rows) {
  const parts = pairs.map(
    ([k, v]) => JSON.stringify(k) + ":" + (v === null ? "[" + rows + "]" : v),
  );
  return "{" + parts.join(",") + "}";
}

const SCHEMA = [
  "CREATE TABLE IF NOT EXISTS board_pages (page INTEGER PRIMARY KEY, json TEXT NOT NULL)",
  // One row per board row: its page and the columns a PATCH looks up. Rows sit in page order,
  // and in order within a page.
  `CREATE TABLE IF NOT EXISTS board_index (id TEXT, page INTEGER NOT NULL, num REAL, capture TEXT,
    names TEXT)`,
  "CREATE INDEX IF NOT EXISTS board_index_id ON board_index (id, page)",
  // Partial indexes, so the many rows without a capture_id or upload names cost no index writes.
  "CREATE INDEX IF NOT EXISTS board_index_capture ON board_index (capture) WHERE capture IS NOT NULL",
  "CREATE INDEX IF NOT EXISTS board_index_names ON board_index (names) WHERE names IS NOT NULL",
  // k 'state' holds the store's state; k 'stages' every id's stage as [[id, stage JSON]] in board
  // order, first row per id, so a PATCH's stage reads cost one SQL row however many there are.
  "CREATE TABLE IF NOT EXISTS board_meta (k TEXT PRIMARY KEY, v TEXT NOT NULL)",
];

/**
 * The room's side of the store. storage is the Durable Object's ctx.storage; waitUntil keeps a
 * read's R2 copy running after the read answers.
 */
export class BoardRows {
  constructor(storage, env, waitUntil = (promise) => promise.catch(() => {})) {
    this.storage = storage;
    this.waitUntil = waitUntil;
    // SQLite rows read and written, summed from each cursor, which the Free plan caps per day.
    this.work = { rowsRead: 0, rowsWritten: 0 };
    this.sql = {
      exec: (query, ...bindings) => {
        const cursor = storage.sql.exec(query, ...bindings);
        const rows = cursor.toArray();
        this.work.rowsRead += cursor.rowsRead ?? 0;
        this.work.rowsWritten += cursor.rowsWritten ?? 0;
        return { toArray: () => rows };
      },
    };
    this.env = env;
    this.memo = { etag: null, blob: null };
    // While an R2 copy is under way, its promise; while a full write is, its promise.
    this.inflight = undefined;
    this.replacing = undefined;
    for (const statement of SCHEMA) this.sql.exec(statement);
  }

  // The store's state: null before the first seed. etag is the board version clients see,
  // r2etag the version of R2's copy the rows match, head the document's top-level pairs,
  // dirty { id: [hash of R2's row, seq] } for each id a row commit changed since that copy,
  // headBefore headKey of R2's copy while a top-level change is uncopied.
  state() {
    return this.load("state");
  }

  // One board_meta value, stored as key and, past META_CHUNK characters, key#0001 and on.
  load(key) {
    const rows = this.sql
      .exec("SELECT v FROM board_meta WHERE k >= ? AND k < ? ORDER BY k", key, key + "$")
      .toArray();
    return rows.length ? JSON.parse(rows.map((r) => r.v).join("")) : null;
  }

  // A top-level field can be megabytes, past SQLite's 2 MB value limit, so long values split.
  save(state, key = "state") {
    const json = JSON.stringify(state);
    const chunks = [];
    for (let i = 0; i < json.length || !chunks.length;) {
      let end = Math.min(i + META_CHUNK, json.length);
      // Never split a surrogate pair, which SQLite would store as two replacement characters.
      if (end < json.length && /[\ud800-\udbff]/.test(json[end - 1])) end--;
      chunks.push(json.slice(i, end));
      i = end;
    }
    const chunkKey = (i) => (i ? `${key}#${String(i).padStart(4, "0")}` : key);
    for (const [i, v] of chunks.entries())
      this.sql.exec(
        "INSERT INTO board_meta (k, v) VALUES (?, ?) ON CONFLICT (k) DO UPDATE SET v = excluded.v",
        chunkKey(i),
        v,
      );
    this.sql.exec(
      "DELETE FROM board_meta WHERE k > ? AND k < ?",
      chunkKey(chunks.length - 1),
      key + "$",
    );
  }

  stages() {
    return new Map(this.load("stages") ?? []);
  }

  // Every page holding a row with one of ids, as Map page -> parsed rows, in page order. Each
  // lookup starts from the given values (CROSS JOIN keeps that order), so it reads the matching
  // index rows and pages only, never a whole table.
  pagesOf(ids) {
    const pages = new Map();
    if (!ids.length) return pages;
    const numbers = this.sql
      .exec(
        "SELECT DISTINCT b.page FROM json_each(?) j CROSS JOIN board_index b ON b.id = j.value",
        JSON.stringify(ids),
      )
      .toArray()
      .map((r) => r.page)
      .sort((a, b) => a - b);
    for (const page of numbers) pages.set(page, null);
    for (const p of this.sql
      .exec(
        "SELECT p.page, p.json FROM json_each(?) j CROSS JOIN board_pages p ON p.page = j.value",
        JSON.stringify(numbers),
      )
      .toArray())
      pages.set(p.page, JSON.parse(p.json));
    for (const [page, rows] of pages) if (rows === null) pages.delete(page);
    return pages;
  }

  // The first row with each id in pages, as Map id -> { pos, row }.
  firstRows(pages, ids) {
    const want = new Set(ids),
      found = new Map();
    for (const [page, rows] of pages)
      for (const [slot, row] of rows.entries()) {
        const id = rowId(row);
        if (id !== null && want.has(id) && !found.has(id))
          found.set(id, { pos: page * 1e6 + slot, row });
      }
    return found;
  }

  // Replaces every page and index row with the board in text. An unreadable board keeps no
  // rows; reads then go to R2, which holds it.
  seed(text, r2etag, etag) {
    const doc = parseDoc(text);
    const old = this.state();
    this.sql.exec("DELETE FROM board_pages");
    this.sql.exec("DELETE FROM board_index");
    const state = {
      etag,
      r2etag,
      readable: doc !== null,
      head: [],
      clean: true,
      bytes: 0,
      count: 0,
      maxNum: null,
      lastPage: 0,
      seq: old?.seq ?? 0,
      dirty: {},
      headSeq: 0,
      headBefore: null,
      // The board is R2's body at r2etag byte for byte, so a read serves that body.
      verbatim: true,
    };
    if (doc) {
      state.head = headPairs(doc);
      const jobs = Array.isArray(doc.jobs) ? doc.jobs : [];
      const ids = new Set();
      const stages = new Map();
      let page = 0,
        rows = [],
        size = 0;
      const writePage = () => {
        this.sql.exec(
          "INSERT INTO board_pages (page, json) VALUES (?, ?)",
          page,
          "[" + rows.join(",") + "]",
        );
      };
      for (const row of jobs) {
        const json = JSON.stringify(row);
        const bytes = byteLength(json);
        if (rows.length && size + bytes > PAGE_BYTES) {
          writePage();
          page++;
          rows = [];
          size = 0;
        }
        rows.push(json);
        size += bytes + 1;
        state.bytes += bytes;
        this.insertIndex(row, page);
        const c = rowColumns(row);
        if (c.num !== null) state.maxNum = Math.max(state.maxNum ?? c.num, c.num);
        if (c.id === null || ids.has(c.id)) state.clean = false;
        if (c.id !== null && !stages.has(c.id)) stages.set(c.id, c.stage);
        ids.add(c.id);
      }
      if (rows.length) writePage();
      this.save([...stages], "stages");
      state.count = jobs.length;
      state.lastPage = page;
    }
    this.save(state);
    this.memo = { etag: null, blob: null };
    return state;
  }

  insertIndex(row, page) {
    const c = rowColumns(row);
    this.sql.exec(
      "INSERT INTO board_index (id, page, num, capture, names) VALUES (?, ?, ?, ?, ?)",
      c.id,
      page,
      c.num,
      c.capture,
      c.names,
    );
  }

  // Applies row changes [{ id, row, add }] to state and the pages: an update rewrites every row
  // with the id, null deletes them, add appends to the last page or a new one. Only the pages
  // and index rows a change touches are written. Returns false, writing nothing, when the board
  // would pass max bytes.
  applyChanges(
    state,
    changes,
    max = Infinity,
    pages = this.pagesOf(changes.filter((c) => !c.add).map((c) => c.id)),
  ) {
    const touched = new Set();
    const index = [];
    // Stage changes, [id, stage JSON or undefined to drop], for the stages row.
    const staged = [];
    let bytes = state.bytes,
      count = state.count,
      maxNum = state.maxNum,
      recount = false;
    for (const c of changes) {
      const next = rowColumns(c.row);
      if (c.add) {
        let last = state.lastPage;
        if (!pages.has(last)) {
          const [p] = this.sql.exec("SELECT json FROM board_pages WHERE page = ?", last).toArray();
          pages.set(last, p ? JSON.parse(p.json) : []);
        }
        const json = JSON.stringify(c.row);
        const size = byteLength(json);
        if (
          pages.get(last).length &&
          byteLength(JSON.stringify(pages.get(last))) + size > PAGE_BYTES
        )
          pages.set(++last, []);
        state.lastPage = last;
        pages.get(last).push(c.row);
        touched.add(last);
        index.push(["insert", c.row, last]);
        if (next.id !== null) staged.push([next.id, next.stage]);
        bytes += size;
        count++;
        if (next.num !== null) maxNum = Math.max(maxNum ?? next.num, next.num);
        continue;
      }
      let old = null;
      for (const [page, rows] of pages) {
        for (let slot = rows.length - 1; slot >= 0; slot--) {
          if (rowId(rows[slot]) !== c.id) continue;
          old ??= rows[slot];
          bytes -= byteLength(JSON.stringify(rows[slot]));
          if (c.row === null) {
            rows.splice(slot, 1);
            count--;
          } else {
            rows[slot] = c.row;
            bytes += byteLength(JSON.stringify(c.row));
          }
          touched.add(page);
        }
      }
      if (old === null) continue;
      const before = rowColumns(old);
      if (c.row === null) {
        index.push(["delete", c.id]);
        staged.push([c.id, undefined]);
        if (before.num !== null && before.num === maxNum) recount = true;
      } else {
        if (!sameColumns(before, next)) index.push(["update", c.row]);
        if (before.stage !== next.stage) staged.push([c.id, next.stage]);
      }
    }
    if (byteLength(boardText(state.head, "")) + bytes + Math.max(count - 1, 0) > max) return false;
    for (const page of touched) {
      const rows = pages.get(page);
      if (rows.length)
        this.sql.exec(
          "INSERT INTO board_pages (page, json) VALUES (?, ?) ON CONFLICT (page) DO UPDATE SET json = excluded.json",
          page,
          JSON.stringify(rows),
        );
      else this.sql.exec("DELETE FROM board_pages WHERE page = ?", page);
    }
    for (const [op, row, page] of index) {
      if (op === "insert") this.insertIndex(row, page);
      else if (op === "delete") this.sql.exec("DELETE FROM board_index WHERE id = ?", row);
      else {
        const c = rowColumns(row);
        this.sql.exec(
          "UPDATE board_index SET num = ?, capture = ?, names = ? WHERE id = ?",
          c.num,
          c.capture,
          c.names,
          c.id,
        );
      }
    }
    if (staged.length) {
      const stages = this.stages();
      for (const [id, stage] of staged)
        if (stage === undefined) stages.delete(id);
        else stages.set(id, stage);
      this.save([...stages], "stages");
    }
    // Only deleting the row with the highest id number scans the index.
    if (recount)
      maxNum = this.sql.exec("SELECT MAX(num) AS m FROM board_index").toArray()[0].m ?? null;
    Object.assign(state, { bytes, count, maxNum });
    this.memo = { etag: null, blob: null };
    return true;
  }

  // Marks each id dirty, keeping the hash of the row R2's copy holds from its first change.
  markDirty(state, ids, beforeOf) {
    for (const id of ids) {
      state.seq++;
      const entry = state.dirty[id];
      state.dirty[id] = [entry ? entry[0] : rowHash(beforeOf(id)), state.seq];
    }
  }

  hasDirty(state = this.state()) {
    return !!state && (Object.keys(state.dirty).length > 0 || state.headBefore != null);
  }

  // Takes in a board.json that something else wrote to R2. Unflushed row commits are applied
  // over it where R2's row is still the one they replaced; a row both sides changed keeps R2's.
  adopt(text, r2etag) {
    const old = this.state();
    const dirty = Object.entries(old?.dirty ?? {});
    if (!dirty.length && old?.headBefore == null) {
      this.seed(text, r2etag, r2etag);
      return;
    }
    const theirs = parseDoc(text);
    if (!theirs) {
      console.warn("board store: R2's board.json is not a readable board; the rows stay");
      this.save({ ...old, r2etag });
      return;
    }
    const ids = dirty.map(([id]) => id);
    const mine = this.firstRows(this.pagesOf(ids), ids);
    const state = this.seed(text, r2etag, newEtag());
    state.seq = old.seq;
    state.verbatim = false;
    const r2rows = this.firstRows(this.pagesOf(ids), ids);
    const kept = [],
      changes = [];
    for (const [id, [before]] of dirty) {
      const r2json = r2rows.has(id) ? JSON.stringify(r2rows.get(id).row) : null;
      if (rowHash(r2json) !== before) {
        kept.push(id);
        continue;
      }
      changes.push({ id, row: mine.get(id)?.row ?? null, add: r2json === null });
    }
    const real = changes.filter((c) => c.row !== null || !c.add);
    this.applyChanges(state, real);
    this.markDirty(
      state,
      real.map((c) => c.id),
      (id) => (r2rows.has(id) ? JSON.stringify(r2rows.get(id).row) : null),
    );
    if (old.headBefore != null) {
      const updated = [pairValue(old.head, "updated"), pairValue(state.head, "updated")]
        .filter((v) => typeof v === "string")
        .sort()
        .pop();
      const r2head = state.head;
      if (headKey(r2head) === old.headBefore) state.head = old.head;
      else kept.push("(top-level fields)");
      if (updated !== undefined)
        state.head = setPair(state.head, "updated", JSON.stringify(updated));
      state.headSeq = ++state.seq;
      state.headBefore = headKey(r2head);
    }
    this.save(state);
    console.warn(
      `board store: merged unflushed rows over an outside write to R2's board.json${
        kept.length ? `; R2 kept ${kept.join(", ")}` : ""
      }`,
    );
  }

  // Seeds from R2 before the first read, takes in the room's own copy that landed without its
  // bookkeeping, and adopts a board.json written there by anything else.
  // A copy or full write under way skips it, since its own put moves R2's version; force is
  // that copy or write itself.
  async sync(force = false) {
    const busy = () => !force && (this.inflight || this.replacing);
    if (busy()) return;
    const head = await this.env.BUCKET.head(BOARD_KEY);
    const state = this.state();
    if (busy() || !head || head.httpEtag === state?.r2etag) return;
    const tag = head.customMetadata;
    if (state && tag?.store === ROOM_COPY && Number(tag.seq) <= state.seq) {
      await this.landed(head.httpEtag, Number(tag.seq));
      return;
    }
    const object = await this.env.BUCKET.get(BOARD_KEY);
    if (!object) return;
    const text = await object.text();
    if (busy() || object.httpEtag === this.state()?.r2etag) return;
    this.storage.transactionSync(() => this.adopt(text, object.httpEtag));
    if (this.hasDirty()) await this.schedule();
  }

  // The room's copy at seq landed in R2 as r2etag, but the room stopped before it recorded so.
  // Rows changed again after seq take R2's row as the one they replaced.
  async landed(r2etag, seq) {
    let flushed = new Map(),
      headAt = null;
    const state = this.state();
    const later = Object.entries(state.dirty).filter(([, [, s]]) => s > seq);
    if (later.length || state.headSeq > seq) {
      const object = await this.env.BUCKET.get(BOARD_KEY);
      const doc = object && object.httpEtag === r2etag ? parseDoc(await object.text()) : null;
      if (!doc) return;
      const jobs = Array.isArray(doc.jobs) ? doc.jobs : [];
      for (const [id] of later) {
        const row = jobs.find((j) => rowId(j) === id);
        flushed.set(id, row === undefined ? null : JSON.stringify(row));
      }
      headAt = headKey(headPairs(doc));
    }
    this.storage.transactionSync(() => this.recordCopy(r2etag, seq, flushed, headAt));
  }

  // Records that R2 holds the room's copy at seq as r2etag. flushed maps each id changed after
  // seq to its JSON in that copy; headAt is the copy's headKey. A copy of version etag, when no
  // commit has moved the board since, is the board byte for byte, so reads serve R2's bytes.
  recordCopy(r2etag, seq, flushed, headAt, etag) {
    const now = this.state();
    if (now.seq < seq) return;
    now.r2etag = r2etag;
    if (etag !== undefined && now.etag === etag) now.verbatim = true;
    for (const [id, [before, s]] of Object.entries(now.dirty))
      if (s <= seq) delete now.dirty[id];
      else if (flushed.has(id)) now.dirty[id] = [rowHash(flushed.get(id)), s];
      else now.dirty[id] = [before, s];
    now.headBefore = now.headSeq <= seq ? null : headAt;
    this.save(now);
  }

  async schedule() {
    if ((await this.storage.getAlarm()) === null)
      await this.storage.setAlarm(Date.now() + FLUSH_MS);
  }

  // The board's bytes at state built from the pages, kept per version in memory, which
  // production loses whenever the hibernating room is evicted, about a minute after its last
  // request.
  blob(state) {
    if (this.memo.etag !== state.etag) this.memo = { etag: state.etag, blob: this.build(state) };
    return this.memo.blob;
  }

  // The board document's bytes from the pages. Each page comes out of SQLite as a blob and goes
  // in as it is, so no row is decoded to a string, joined, and encoded again, about 40% of the
  // build's CPU under workerd.
  build(state) {
    // A raw NUL never appears in JSON text, so it marks where the rows go.
    const [open, close] = boardText(state.head, "\0").split("\0");
    const parts = [open];
    if (close !== undefined) {
      for (const { json } of this.sql
        .exec("SELECT CAST(json AS BLOB) AS json FROM board_pages ORDER BY page")
        .toArray()) {
        // workerd gives an ArrayBuffer, which this views; Node's SQLite a Uint8Array.
        const rows = new Uint8Array(json).subarray(1, -1);
        if (!rows.length) continue;
        if (parts.length > 1) parts.push(",");
        parts.push(rows);
      }
      parts.push(close);
    }
    return new Blob(parts);
  }

  async head() {
    await this.sync();
    const state = this.state();
    return state ? { etag: state.etag } : null;
  }

  // The board as { etag, body }, its bytes undecoded, or null when there is none. While R2's
  // copy is the board, its body streams through, which costs the room almost no CPU however it
  // was evicted; otherwise the bytes come from the memo or the pages. A BOARD_DEBUG var logs
  // which one each read took.
  async body() {
    await this.sync();
    let state = this.state();
    if (!state) return null;
    const debug = (how) =>
      this.env.BOARD_DEBUG && console.log(`board store: read ${how} for ${state.etag}`);
    if (!state.readable || (state.verbatim && this.memo.etag !== state.etag)) {
      const object = await this.env.BUCKET.get(BOARD_KEY);
      if (!state.readable) return object && { etag: state.etag, body: object.body };
      if (object?.httpEtag === state.r2etag) {
        debug("R2's bytes");
        return { etag: state.etag, body: object.body };
      }
      // R2 moved during the get, so the board may have too: build from the state as it is now,
      // never an old head over the current pages.
      state = this.state();
    }
    const built = this.memo.etag !== state.etag;
    debug(built ? "the pages" : "the memo");
    const body = this.blob(state);
    // The first read that builds a version copies those bytes to R2 at once, the alarm's copy
    // early, so later reads stream R2's and the alarm finds nothing to do. The read does not
    // wait on the put; a put that fails or loses its condition leaves the alarm to retry.
    if (built && this.hasDirty(state)) this.waitUntil(this.flush());
    return { etag: state.etag, body };
  }

  async read() {
    const board = await this.body();
    return board && { etag: board.etag, text: await new Response(board.body).text() };
  }

  // What a row PATCH decides from, at the current version, or null when at names another.
  //   ids: the rows to return; stages: [id, stage] reads, whose ids come back in stale when the
  //   row is missing or its stage differs; captures: capture ids, with the first row holding
  //   each; names: upload names, with the ones a stored row carries.
  async rows(q, at) {
    if (at === undefined) await this.sync();
    const state = this.state();
    if (at !== undefined && (state?.etag ?? null) !== at) return null;
    if (!state)
      return {
        etag: null,
        readable: true,
        head: null,
        clean: true,
        maxNum: null,
        rows: [],
        stale: (q.stages ?? []).map(([id]) => id),
        captures: [],
        names: [],
      };
    if (!state.readable) return { etag: state.etag, readable: false };
    const want = new Set(q.ids ?? []);
    const stale = [];
    if (q.stages?.length) {
      const found = this.stages();
      for (const [id, stage] of q.stages)
        if (found.get(id) !== JSON.stringify(stage)) {
          stale.push(id);
          if (found.has(id)) want.add(id);
        }
    }
    const holders = new Map();
    if (q.captures?.length)
      for (const r of this.sql
        .exec(
          `SELECT b.capture, b.id, b.page FROM json_each(?) j
            CROSS JOIN board_index b INDEXED BY board_index_capture ON b.capture = j.value`,
          JSON.stringify(q.captures),
        )
        .toArray()
        .sort((a, b) => a.page - b.page))
        if (!holders.has(r.capture)) holders.set(r.capture, r.id);
    // A capture's first holder by board order: rows on one page are ordered by the page itself.
    const pages = this.pagesOf([...new Set([...want, ...[...holders.values()].filter(Boolean)])]);
    const captures = [];
    if (holders.size) {
      const order = [];
      for (const [, rows] of pages) for (const row of rows) order.push(row);
      for (const c of q.captures) {
        const first = order.find(
          (row) => typeof row?.capture_id === "string" && row.capture_id === c,
        );
        if (first === undefined) continue;
        captures.push([c, first.id]);
        if (rowId(first) !== null) want.add(first.id);
      }
    }
    const found = this.firstRows(pages, [...want]);
    // The rows already fetched vouch for their own names; only the rest cost an index scan.
    const carried = new Set([...found.values()].flatMap(({ row }) => rowNames(row)));
    const names = (q.names ?? []).filter((n) => carried.has(n));
    const rest = (q.names ?? []).filter((n) => !carried.has(n));
    if (rest.length)
      for (const r of this.sql
        .exec(
          `SELECT DISTINCT j.value AS name FROM board_index r INDEXED BY board_index_names, json_each(r.names) j
            WHERE r.names IS NOT NULL AND j.value IN (SELECT value FROM json_each(?))`,
          JSON.stringify(rest),
        )
        .toArray())
        names.push(r.name);
    return {
      etag: state.etag,
      readable: true,
      head: headObject(state.head),
      clean: state.clean,
      maxNum: state.maxNum,
      rows: [...found.values()].map(({ pos, row }) => ({ pos, json: JSON.stringify(row) })),
      stale,
      captures,
      names,
    };
  }

  // Commits a row PATCH built on base: changes is [{ id, row, add }], row null to delete.
  // { etag }, { moved: true } when base is not the current version, or { tooLarge: true }
  // when the board would pass max bytes.
  async commit({ base, changes, updated, max }) {
    while (this.replacing) await this.replacing;
    const result = this.storage.transactionSync(() => {
      const old = this.state();
      if ((old?.etag ?? null) !== base || (old && !old.readable)) return { moved: true };
      // A patch on an absent board builds on { updated: "", jobs: [] }.
      const state = old ?? {
        etag: null,
        r2etag: null,
        readable: true,
        head: [["updated", '""']],
        clean: true,
        bytes: 0,
        count: 0,
        maxNum: null,
        lastPage: 0,
        seq: 0,
        dirty: {},
        headSeq: 0,
        headBefore: null,
      };
      const head = state.head;
      state.head = setPair(state.head, "jobs", null);
      if (updated !== undefined)
        state.head = setPair(state.head, "updated", JSON.stringify(updated));
      const ids = changes.filter((c) => !c.add).map((c) => c.id);
      const pages = this.pagesOf(ids);
      const before = this.firstRows(pages, ids);
      if (!this.applyChanges(state, changes, max, pages)) return { tooLarge: true };
      this.markDirty(
        state,
        changes.map((c) => c.id),
        (id) => (before.has(id) ? JSON.stringify(before.get(id).row) : null),
      );
      if (JSON.stringify(state.head) !== JSON.stringify(head)) {
        state.headBefore ??= headKey(head);
        state.headSeq = state.seq;
      }
      state.etag = newEtag();
      state.verbatim = false;
      this.save(state);
      return { etag: state.etag };
    });
    if (result.etag) await this.schedule();
    return result;
  }

  // Replaces the whole board under cond { ifMatch } or { ifNoneMatch: true }. It copies the board
  // to R2 first, on the condition that R2 still holds the room's last copy, so a write an older
  // Worker landed there wins and this one gets null, as a failed cond does. { etag }, null, or
  // { tooLarge: true } for a row over MAX_ROW bytes.
  async replace(text, cond = {}) {
    while (this.replacing) await this.replacing;
    const run = this.replaceOnce(text, cond);
    this.replacing = run.then(
      () => {},
      () => {},
    );
    try {
      return await run;
    } finally {
      this.replacing = undefined;
    }
  }

  async replaceOnce(text, cond) {
    const doc = parseDoc(text);
    if (!doc) throw new Error("board store: a full write needs a board document");
    // One SQLite page holds at most 2 MB. A PATCH body is capped at 1 MiB, so only a full
    // write can bring a row this large.
    const jobs = Array.isArray(doc.jobs) ? doc.jobs : [];
    if (jobs.some((j) => byteLength(JSON.stringify(j) ?? "") > MAX_ROW)) return { tooLarge: true };
    while (this.inflight) await this.inflight;
    await this.sync(true);
    const state = this.state();
    const current = state?.etag ?? null;
    if (cond.ifMatch !== undefined && current !== cond.ifMatch) return null;
    if (cond.ifNoneMatch && current !== null) return null;
    const seq = (state?.seq ?? 0) + 1;
    const r2etag = state?.r2etag ?? null;
    // R2 keeps the body as sent, so a read of this version answers with the same bytes.
    const put = await this.env.BUCKET.put(BOARD_KEY, text, {
      onlyIf: new Headers(r2etag ? { "If-Match": r2etag } : { "If-None-Match": "*" }),
      httpMetadata: { contentType: "application/json" },
      customMetadata: { store: ROOM_COPY, seq: String(seq) },
    });
    if (!put) {
      await this.sync(true);
      return null;
    }
    const etag = newEtag();
    this.storage.transactionSync(() => {
      const next = this.applyReplace(state, doc, put.httpEtag, etag);
      Object.assign(next, { seq, dirty: {}, headSeq: 0, headBefore: null, verbatim: true });
      this.save(next);
    });
    return { etag };
  }

  // The board doc in place of state's: as row changes when it keeps the stored order, dropping
  // rows and appending new ones, else by reseeding every page.
  applyReplace(state, doc, r2etag, etag) {
    const jobs = Array.isArray(doc.jobs) ? doc.jobs : [];
    if (state?.readable && state.clean) {
      const stored = [];
      for (const p of this.sql.exec("SELECT json FROM board_pages ORDER BY page").toArray())
        stored.push(...JSON.parse(p.json));
      const next = new Map();
      for (const row of jobs) if (rowId(row) !== null) next.set(row.id, row);
      const kept = stored.filter((row) => next.has(row.id));
      const ordered =
        next.size === jobs.length && kept.every((row, i) => rowId(jobs[i]) === row.id);
      if (ordered) {
        const storedIds = new Set(stored.map((row) => row.id));
        const changes = [];
        for (const row of stored)
          if (!next.has(row.id)) changes.push({ id: row.id, row: null, add: false });
          else if (JSON.stringify(next.get(row.id)) !== JSON.stringify(row))
            changes.push({ id: row.id, row: next.get(row.id), add: false });
        for (const row of jobs.slice(kept.length))
          if (!storedIds.has(row.id)) changes.push({ id: row.id, row, add: true });
        const out = { ...state, head: headPairs(doc), etag, r2etag };
        this.applyChanges(out, changes);
        return out;
      }
    }
    return this.seed(JSON.stringify(doc), r2etag, etag);
  }

  // Copies the board to R2 after row commits, tagged with the room's seq, on the condition that
  // R2 still holds the room's last copy; otherwise R2's board is adopted and copied later.
  flush() {
    if (this.replacing) return Promise.resolve();
    this.inflight ??= this.flushOnce().finally(() => {
      this.inflight = undefined;
    });
    return this.inflight;
  }

  async flushOnce() {
    const state = this.state();
    if (!state?.readable || !this.hasDirty(state)) return;
    // Each dirty row as this copy holds it, the row a later merge compares R2's against.
    const ids = Object.keys(state.dirty);
    const rows = this.firstRows(this.pagesOf(ids), ids);
    const flushed = new Map(
      ids.map((id) => [id, rows.has(id) ? JSON.stringify(rows.get(id).row) : null]),
    );
    const put = await this.env.BUCKET.put(BOARD_KEY, await this.blob(state).arrayBuffer(), {
      onlyIf: new Headers(state.r2etag ? { "If-Match": state.r2etag } : { "If-None-Match": "*" }),
      httpMetadata: { contentType: "application/json" },
      customMetadata: { store: ROOM_COPY, seq: String(state.seq) },
    });
    if (put)
      this.storage.transactionSync(() =>
        this.recordCopy(put.httpEtag, state.seq, flushed, headKey(state.head), state.etag),
      );
    else await this.sync(true);
    if (this.hasDirty()) await this.schedule();
  }
}

// The Worker's handle on the room's store, or on R2 alone. Unit tests stub the room with
// fetch alone; a real Durable Object stub answers every method name.
export function boardStore(env) {
  const stub = env.BoardEvents?.get(env.BoardEvents.idFromName(ROOM));
  if (typeof stub?.boardRows !== "function") return r2Store(env);
  return {
    head: () => stub.boardHead(),
    read: () => stub.boardRead(),
    body: async () => {
      const response = await stub.fetch("https://board-events.invalid/board");
      return response.status === 404
        ? null
        : { etag: response.headers.get("ETag"), body: response.body };
    },
    rows: async (q, at) => {
      const snap = await stub.boardRows(q, at);
      if (snap?.rows)
        snap.rows = snap.rows.map(({ pos, json }) => ({ pos, row: JSON.parse(json) }));
      return snap;
    },
    commit: (c) => stub.boardCommit(c),
    replace: (text, cond) => stub.boardReplace(text, cond),
  };
}

// The board read as { data, etag }, or null when it is missing or not JSON, as readJson reads
// any other key.
export async function readBoardJson(env) {
  const object = await boardStore(env).read();
  if (!object) return null;
  try {
    return { data: JSON.parse(object.text), etag: object.etag };
  } catch {
    return null;
  }
}

// The same store over R2's board.json alone: every read parses the whole board and every
// write puts it.
function r2Store(env) {
  let last = null;
  const load = async () => {
    const object = await env.BUCKET.get(BOARD_KEY);
    if (!object) return { etag: null, doc: null, absent: true };
    return { etag: object.httpEtag, doc: parseDoc(await object.text()) };
  };
  const condition = (cond = {}) =>
    cond.ifMatch !== undefined
      ? new Headers({ "If-Match": cond.ifMatch })
      : cond.ifNoneMatch
        ? new Headers({ "If-None-Match": "*" })
        : undefined;
  return {
    async head() {
      const head = await env.BUCKET.head(BOARD_KEY);
      return head && { etag: head.httpEtag };
    },
    async read() {
      const object = await env.BUCKET.get(BOARD_KEY);
      return object && { etag: object.httpEtag, text: await object.text() };
    },
    async body() {
      const object = await env.BUCKET.get(BOARD_KEY);
      return object && { etag: object.httpEtag, body: object.body };
    },
    async rows(q, at) {
      if (at === undefined || last?.etag !== at) last = await load();
      if (at !== undefined && last.etag !== at) return null;
      if (last.etag && !last.doc) return { etag: last.etag, readable: false };
      const jobs = Array.isArray(last.doc?.jobs) ? last.doc.jobs : [];
      const byId = new Map(),
        captured = new Map(),
        ids = new Set();
      let maxNum = null,
        clean = true;
      for (const [pos, j] of jobs.entries()) {
        if (typeof j?.id !== "string" || ids.has(j.id)) clean = false;
        ids.add(j?.id);
        if (typeof j?.id === "string" && !byId.has(j.id)) byId.set(j.id, { pos, row: j });
        if (typeof j?.capture_id === "string" && !captured.has(j.capture_id))
          captured.set(j.capture_id, j.id);
        const m = typeof j?.id === "string" && ID_PREFIX.exec(j.id);
        if (m) maxNum = Math.max(maxNum ?? -Infinity, Number(m[1]));
      }
      const want = new Set(q.ids ?? []);
      const stale = [];
      for (const [id, stage] of q.stages ?? [])
        if ((byId.get(id)?.row.stage ?? null) !== stage || !byId.has(id)) {
          stale.push(id);
          want.add(id);
        }
      const captures = [];
      for (const c of q.captures ?? [])
        if (captured.has(c)) {
          captures.push([c, captured.get(c)]);
          want.add(captured.get(c));
        }
      const stored = storedNames(jobs);
      const { jobs: _, ...head } = last.doc ?? {};
      return {
        etag: last.etag,
        readable: true,
        head: last.doc ? head : null,
        clean,
        maxNum,
        rows: [...want].filter((id) => byId.has(id)).map((id) => byId.get(id)),
        stale,
        captures,
        names: (q.names ?? []).filter((n) => stored.has(n)),
      };
    },
    async commit({ base, changes, updated, max }) {
      const current = last?.etag === base ? last : await load();
      if (current.etag !== base) return { moved: true };
      const doc = current.doc ?? { updated: "", jobs: [] };
      const byId = new Map(changes.filter((c) => !c.add).map((c) => [c.id, c]));
      const jobs = [];
      for (const j of Array.isArray(doc.jobs) ? doc.jobs : []) {
        const c = byId.get(j?.id);
        if (!c) jobs.push(j);
        else if (c.row) jobs.push(c.row);
      }
      for (const c of changes) if (c.add) jobs.push(c.row);
      const bytes = encoder.encode(JSON.stringify({ ...doc, jobs, updated }));
      if (bytes.length > max) return { tooLarge: true };
      const object = await env.BUCKET.put(BOARD_KEY, bytes, {
        onlyIf: condition(base ? { ifMatch: base } : { ifNoneMatch: true }),
        httpMetadata: { contentType: "application/json" },
      });
      last = null;
      return object ? { etag: object.httpEtag } : { moved: true };
    },
    async replace(text, cond) {
      const onlyIf = condition(cond);
      const object = await env.BUCKET.put(BOARD_KEY, text, {
        ...(onlyIf && { onlyIf }),
        httpMetadata: { contentType: "application/json" },
      });
      return object && { etag: object.httpEtag };
    },
  };
}
