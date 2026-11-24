// Board storage. The board's rows live in the BoardEvents room's SQLite, one SQL row per board
// row, so a row PATCH reads and writes only the rows it names. GET /board.json is built from the
// rows and cached per version. R2's board.json is a copy the room writes behind each change: at
// once for a full write, within FLUSH_MS of a row commit. The room adopts a board.json that
// something else wrote to R2, such as a Worker version from before this store or an operator's
// restore, and merges its own unflushed rows over it row by row.
//
// boardStore(env) is the Worker's handle. Without a room binding (unit tests that give the Worker
// a memory bucket alone, or a room stub with fetch alone) it reads and writes R2's board.json
// directly, the whole object each time, as the Worker did before this store.

export const BOARD_KEY = "board.json";
export const ROOM = "main";
export const FLUSH_MS = 5000;
const MAX_ROW = 1 << 20;

const ID_PREFIX = /^(\d{3,})-/;
const encoder = new TextEncoder();
const byteLength = (text) => encoder.encode(text).length;
const isDoc = (doc) => typeof doc === "object" && doc !== null && !Array.isArray(doc);
const newEtag = () => `"b${crypto.randomUUID().replaceAll("-", "")}"`;

function parseDoc(text) {
  try {
    const doc = JSON.parse(text);
    return isDoc(doc) ? doc : null;
  } catch {
    return null;
  }
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

// The indexed columns of one board row.
function rowColumns(row) {
  const json = JSON.stringify(row);
  const id = typeof row?.id === "string" ? row.id : null;
  const m = id !== null && ID_PREFIX.exec(id);
  const names = rowNames(row);
  return {
    id,
    json,
    stage: id !== null ? JSON.stringify(row.stage ?? null) : null,
    num: m ? Number(m[1]) : null,
    capture: typeof row?.capture_id === "string" ? row.capture_id : null,
    names: names.length ? JSON.stringify(names) : null,
    bytes: byteLength(json),
  };
}

// A document's top-level keys in order, each with its value's JSON. An array jobs value is
// null here: the rows hold it.
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

// The board document's text: JSON.stringify of the document the head and rows make.
function boardText(pairs, rowTexts) {
  const parts = pairs.map(
    ([k, v]) => JSON.stringify(k) + ":" + (v === null ? "[" + rowTexts.join(",") + "]" : v),
  );
  return "{" + parts.join(",") + "}";
}

const SCHEMA = [
  `CREATE TABLE IF NOT EXISTS board_rows (pos INTEGER PRIMARY KEY, id TEXT, json TEXT NOT NULL,
    stage TEXT, num REAL, capture TEXT, names TEXT, bytes INTEGER NOT NULL)`,
  "CREATE INDEX IF NOT EXISTS board_rows_id ON board_rows (id)",
  "CREATE INDEX IF NOT EXISTS board_rows_num ON board_rows (num)",
  "CREATE INDEX IF NOT EXISTS board_rows_capture ON board_rows (capture)",
  "CREATE TABLE IF NOT EXISTS board_meta (k TEXT PRIMARY KEY, v TEXT NOT NULL)",
  // Ids a row commit changed since the last flush, each with its JSON in R2's copy (null when
  // R2's copy has no such row), so a merge can tell whose change a row is.
  "CREATE TABLE IF NOT EXISTS board_dirty (id TEXT PRIMARY KEY, before TEXT, seq INTEGER NOT NULL)",
];

/** The room's side of the store. storage is the Durable Object's ctx.storage. */
export class BoardRows {
  constructor(storage, env) {
    this.storage = storage;
    this.sql = storage.sql;
    this.env = env;
    this.memo = { etag: null, text: "" };
    this.flushing = false;
    this.inflight = undefined;
    for (const statement of SCHEMA) this.sql.exec(statement);
  }

  // The store's state: null before the first seed. etag is the board version clients see,
  // r2etag the version of R2's copy the rows match, head the document's top-level pairs.
  // headBefore is headKey of R2's copy while a commit's head change is unflushed.
  state() {
    const [row] = this.sql.exec("SELECT v FROM board_meta WHERE k = 'state'").toArray();
    return row ? JSON.parse(row.v) : null;
  }

  save(state) {
    this.sql.exec(
      "INSERT INTO board_meta (k, v) VALUES ('state', ?) ON CONFLICT (k) DO UPDATE SET v = excluded.v",
      JSON.stringify(state),
    );
  }

  insert(pos, row) {
    const c = rowColumns(row);
    this.sql.exec(
      "INSERT INTO board_rows (pos, id, json, stage, num, capture, names, bytes) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
      pos,
      c.id,
      c.json,
      c.stage,
      c.num,
      c.capture,
      c.names,
      c.bytes,
    );
    return c.bytes;
  }

  // The JSON of the first row with this id, or null.
  firstJson(id) {
    const [row] = this.sql
      .exec("SELECT json FROM board_rows WHERE id = ? ORDER BY pos LIMIT 1", id)
      .toArray();
    return row ? row.json : null;
  }

  // Replaces every row with the board in text. An unreadable board keeps no rows; reads then
  // go to R2, which holds it.
  seed(text, r2etag, etag) {
    const doc = parseDoc(text);
    const old = this.state();
    this.sql.exec("DELETE FROM board_rows");
    this.sql.exec("DELETE FROM board_dirty");
    const state = {
      etag,
      r2etag,
      readable: doc !== null,
      head: [],
      clean: true,
      bytes: 0,
      count: 0,
      seq: old?.seq ?? 0,
      headSeq: 0,
      headBefore: null,
    };
    if (doc) {
      state.head = headPairs(doc);
      const jobs = Array.isArray(doc.jobs) ? doc.jobs : [];
      const ids = new Set();
      for (const [pos, row] of jobs.entries()) {
        state.bytes += this.insert(pos, row);
        if (typeof row?.id !== "string" || ids.has(row.id)) state.clean = false;
        ids.add(row?.id);
      }
      state.count = jobs.length;
    }
    this.save(state);
    this.memo = { etag: null, text: "" };
    return state;
  }

  // Takes in a board.json that something else wrote to R2. Unflushed row commits are applied
  // over it where R2's row is still the one they replaced; a row both sides changed keeps R2's.
  adopt(text, r2etag) {
    const old = this.state();
    const dirty = old ? this.sql.exec("SELECT id, before FROM board_dirty").toArray() : [];
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
    const mine = new Map(dirty.map((d) => [d.id, this.firstJson(d.id)]));
    const state = this.seed(text, r2etag, newEtag());
    const kept = [];
    let seq = old.seq;
    for (const d of dirty) {
      const r2row = this.firstJson(d.id);
      if (r2row !== d.before) {
        kept.push(d.id);
        continue;
      }
      const json = mine.get(d.id);
      const delta = this.applyRow(d.id, json === null ? null : JSON.parse(json), r2row === null);
      state.bytes += delta.bytes;
      state.count += delta.count;
      this.markDirty(d.id, r2row, ++seq);
    }
    if (old.headBefore != null) {
      const updated = [pairValue(old.head, "updated"), pairValue(state.head, "updated")]
        .filter((v) => typeof v === "string")
        .sort()
        .pop();
      if (headKey(state.head) === old.headBefore) state.head = old.head;
      else kept.push("(top-level fields)");
      if (updated !== undefined)
        state.head = setPair(state.head, "updated", JSON.stringify(updated));
      state.headSeq = ++seq;
      state.headBefore = headKey(headPairs(theirs));
    }
    state.seq = seq;
    this.save(state);
    console.warn(
      `board store: merged unflushed rows over an outside write to R2's board.json${
        kept.length ? `; R2 kept ${kept.join(", ")}` : ""
      }`,
    );
  }

  // Writes one board row by id: updates every row with the id, deletes them for null, or
  // appends when add is set. The change in stored bytes and in row count.
  applyRow(id, row, add) {
    const old = add ? [] : this.sql.exec("SELECT bytes FROM board_rows WHERE id = ?", id).toArray();
    const before = old.reduce((sum, r) => sum + r.bytes, 0);
    if (row === null) {
      this.sql.exec("DELETE FROM board_rows WHERE id = ?", id);
      return { bytes: -before, count: -old.length };
    }
    if (add) {
      const [{ next }] = this.sql
        .exec("SELECT COALESCE(MAX(pos), -1) + 1 AS next FROM board_rows")
        .toArray();
      return { bytes: this.insert(next, row), count: 1 };
    }
    const c = rowColumns(row);
    this.sql.exec(
      "UPDATE board_rows SET json = ?, stage = ?, num = ?, capture = ?, names = ?, bytes = ? WHERE id = ?",
      c.json,
      c.stage,
      c.num,
      c.capture,
      c.names,
      c.bytes,
      id,
    );
    return { bytes: c.bytes * old.length - before, count: 0 };
  }

  markDirty(id, before, seq) {
    this.sql.exec(
      "INSERT INTO board_dirty (id, before, seq) VALUES (?, ?, ?) ON CONFLICT (id) DO UPDATE SET seq = excluded.seq",
      id,
      before,
      seq,
    );
  }

  // Seeds from R2 before the first read, and adopts a board.json written there since.
  async sync() {
    if (this.flushing) return;
    const head = await this.env.BUCKET.head(BOARD_KEY);
    const state = this.state();
    if (this.flushing || !head || head.httpEtag === state?.r2etag) return;
    const object = await this.env.BUCKET.get(BOARD_KEY);
    if (!object) return;
    const text = await object.text();
    if (this.flushing || object.httpEtag === this.state()?.r2etag) return;
    this.storage.transactionSync(() => this.adopt(text, object.httpEtag));
    if (this.hasDirty()) await this.schedule();
  }

  hasDirty() {
    const state = this.state();
    return (
      state?.headBefore != null ||
      this.sql.exec("SELECT 1 FROM board_dirty LIMIT 1").toArray().length > 0
    );
  }

  async schedule() {
    if ((await this.storage.getAlarm()) === null)
      await this.storage.setAlarm(Date.now() + FLUSH_MS);
  }

  text(state) {
    if (this.memo.etag !== state.etag) {
      const rows = this.sql.exec("SELECT json FROM board_rows ORDER BY pos").toArray();
      this.memo = {
        etag: state.etag,
        text: boardText(
          state.head,
          rows.map((r) => r.json),
        ),
      };
    }
    return this.memo.text;
  }

  async head() {
    await this.sync();
    const state = this.state();
    return state ? { etag: state.etag } : null;
  }

  async read() {
    await this.sync();
    const state = this.state();
    if (!state) return null;
    if (state.readable) return { etag: state.etag, text: this.text(state) };
    const object = await this.env.BUCKET.get(BOARD_KEY);
    return object ? { etag: state.etag, text: await object.text() } : null;
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
      const found = new Map();
      for (const r of this.sql
        .exec(
          "SELECT id, stage FROM board_rows WHERE id IN (SELECT value FROM json_each(?)) ORDER BY pos",
          JSON.stringify(q.stages.map(([id]) => id)),
        )
        .toArray())
        if (!found.has(r.id)) found.set(r.id, r.stage);
      for (const [id, stage] of q.stages)
        if (found.get(id) !== JSON.stringify(stage)) {
          stale.push(id);
          if (found.has(id)) want.add(id);
        }
    }
    const captures = [];
    if (q.captures?.length) {
      const seen = new Set();
      for (const r of this.sql
        .exec(
          "SELECT capture, id FROM board_rows WHERE capture IN (SELECT value FROM json_each(?)) ORDER BY pos",
          JSON.stringify(q.captures),
        )
        .toArray())
        if (!seen.has(r.capture)) {
          seen.add(r.capture);
          captures.push([r.capture, r.id]);
          if (r.id !== null) want.add(r.id);
        }
    }
    const names = q.names?.length
      ? this.sql
          .exec(
            "SELECT DISTINCT j.value AS name FROM board_rows r, json_each(r.names) j WHERE r.names IS NOT NULL AND j.value IN (SELECT value FROM json_each(?))",
            JSON.stringify(q.names),
          )
          .toArray()
          .map((r) => r.name)
      : [];
    const rows = want.size
      ? this.sql
          .exec(
            "SELECT pos, json FROM board_rows WHERE id IN (SELECT value FROM json_each(?)) ORDER BY pos",
            JSON.stringify([...want]),
          )
          .toArray()
      : [];
    const [{ maxNum }] = this.sql.exec("SELECT MAX(num) AS maxNum FROM board_rows").toArray();
    return {
      etag: state.etag,
      readable: true,
      head: headObject(state.head),
      clean: state.clean,
      maxNum: maxNum ?? null,
      rows: rows.map((r) => ({ pos: r.pos, json: r.json })),
      stale,
      captures,
      names,
    };
  }

  // Commits a row PATCH built on base: changes is [{ id, row, add }], row null to delete.
  // { etag }, { moved: true } when base is not the current version, or { tooLarge: true }
  // when the board would pass max bytes.
  async commit({ base, changes, updated, max }) {
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
        seq: 0,
        headSeq: 0,
        headBefore: null,
      };
      const head = state.head;
      state.head = setPair(state.head, "jobs", null);
      if (updated !== undefined)
        state.head = setPair(state.head, "updated", JSON.stringify(updated));
      let bytes = state.bytes,
        count = state.count;
      for (const c of changes) {
        const old = c.add
          ? []
          : this.sql.exec("SELECT bytes FROM board_rows WHERE id = ?", c.id).toArray();
        const size = c.row === null ? 0 : byteLength(JSON.stringify(c.row));
        bytes += size * (c.add ? 1 : old.length) - old.reduce((sum, r) => sum + r.bytes, 0);
        count += c.add ? 1 : c.row === null ? -old.length : 0;
      }
      if (byteLength(boardText(state.head, [])) + bytes + Math.max(count - 1, 0) > max)
        return { tooLarge: true };
      let seq = state.seq;
      for (const c of changes) {
        const before = c.add ? null : this.firstJson(c.id);
        this.applyRow(c.id, c.row, c.add);
        this.markDirty(c.id, before, ++seq);
      }
      if (JSON.stringify(state.head) !== JSON.stringify(head)) {
        state.headBefore ??= headKey(head);
        state.headSeq = seq;
      }
      Object.assign(state, { etag: newEtag(), bytes, count, seq });
      this.save(state);
      return { etag: state.etag };
    });
    if (result.etag) await this.schedule();
    return result;
  }

  // Replaces the whole board, under cond { ifMatch } or { ifNoneMatch: true }, and copies it to
  // R2 before it answers. { etag }, null when cond fails, or { tooLarge: true } for a row over
  // MAX_ROW bytes.
  async replace(text, cond = {}) {
    await this.sync();
    const doc = parseDoc(text);
    if (!doc) throw new Error("board store: a full write needs a board document");
    // One SQLite row holds at most 2 MB. A PATCH body is capped at 1 MiB, so only a full
    // write can bring a row this large.
    if (
      Array.isArray(doc.jobs) &&
      doc.jobs.some((j) => byteLength(JSON.stringify(j) ?? "") > MAX_ROW)
    )
      return { tooLarge: true };
    const state = this.state();
    const current = state?.etag ?? null;
    if (cond.ifMatch !== undefined && current !== cond.ifMatch) return null;
    if (cond.ifNoneMatch && current !== null) return null;
    const etag = newEtag();
    this.storage.transactionSync(() => {
      const next = this.seed(text, state?.r2etag ?? null, etag);
      // Unflushed until the copy below lands, so a flush already under way cannot clear it.
      next.seq++;
      Object.assign(next, { headSeq: next.seq, headBefore: "" });
      this.save(next);
    });
    while (this.inflight) await this.inflight;
    await this.flush(true);
    return { etag };
  }

  // Copies the board to R2. A forced flush writes over whatever R2 holds; otherwise R2 must
  // still hold the copy the rows came from, else its board is adopted and flushed later.
  flush(force = false) {
    this.inflight ??= this.flushOnce(force).finally(() => {
      this.inflight = undefined;
    });
    return this.inflight;
  }

  async flushOnce(force) {
    const state = this.state();
    if (!state?.readable || (!force && !this.hasDirty())) return;
    // Each dirty row as this copy holds it, the row a later merge compares R2's against.
    const flushed = new Map(
      this.sql
        .exec("SELECT id FROM board_dirty")
        .toArray()
        .map((d) => [d.id, this.firstJson(d.id)]),
    );
    this.flushing = true;
    let put;
    try {
      const onlyIf = force
        ? undefined
        : new Headers(state.r2etag ? { "If-Match": state.r2etag } : { "If-None-Match": "*" });
      put = await this.env.BUCKET.put(BOARD_KEY, this.text(state), {
        ...(onlyIf && { onlyIf }),
        httpMetadata: { contentType: "application/json" },
      });
    } finally {
      this.flushing = false;
    }
    if (put)
      this.storage.transactionSync(() => {
        const now = this.state();
        now.r2etag = put.httpEtag;
        for (const d of this.sql.exec("SELECT id, seq FROM board_dirty").toArray())
          if (d.seq <= state.seq) this.sql.exec("DELETE FROM board_dirty WHERE id = ?", d.id);
          else if (flushed.has(d.id))
            this.sql.exec(
              "UPDATE board_dirty SET before = ? WHERE id = ?",
              flushed.get(d.id),
              d.id,
            );
        if (now.headSeq <= state.seq) now.headBefore = null;
        else now.headBefore = headKey(state.head);
        this.save(now);
      });
    else await this.sync();
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
