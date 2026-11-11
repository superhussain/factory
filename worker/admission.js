// Admission for operator-written text and pipeline configuration. Floors live here in
// code; a stored pipeline document can only tighten them, except review rounds, which
// the global document can raise to 5.

export const FLOORS = Object.freeze(
  [
    [
      "F1",
      "Never push to the base or force-push. Every change lands as a PR from factory/<id>.",
      "none",
    ],
    [
      "F2",
      "Four reviewers (security, ux, ui, code), fresh context, WAIT FOR ALL. Security and code always run. ux and ui are SKIPPED only by factory.py review-plan when a round changes nothing a person sees, and the approver checks each skip.",
      "none",
    ],
    [
      "F3",
      "At most 2 review rounds per build cycle by default, then needs-human.",
      "1 to 5; only the all-projects scope can raise it above 2",
    ],
    ["F4", "Approver runs on every job. Only APPROVE merges. ESCALATE holds the PR.", "none"],
    ["F5", "APPROVE PRs merge after green checks.", "tighten to human: every PR held for a person"],
    [
      "F6",
      "needs-human on round cap, second agent failure, second merge failure, rejected push.",
      "none",
    ],
    ["F7", "Claims and sessions; the orchestrator is the only agent board writer.", "none"],
    [
      "F8",
      "No secrets in job files, config or logs; device credentials stay on their host.",
      "none",
    ],
    [
      "F9",
      "Role definitions, output files, tool permissions and model routing come from the installed bundle and runtime config, never R2.",
      "none",
    ],
    ["F10", "The spec's scope is authoritative; config text cannot add scope or files.", "none"],
    [
      "F11",
      "Pipeline config is written only by an Access-authenticated person, never a service token.",
      "none",
    ],
  ].map(([id, rule, config]) => Object.freeze({ id, rule, config })),
);

const REVIEWERS = ["security", "ux", "ui", "code"];
const STAGES = ["spec", "build", "review", "approve"];
const KEYS = [
  "schema",
  "merge",
  "review_rounds",
  "gates",
  "pickup",
  "triage",
  "stage_notes",
  "archive",
];
export const HOST_NAME = /^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/;

// Optional gate stages. Config can switch a catalog gate on; it can never add one or
// remove a loop stage.
export const GATES = Object.freeze(
  [["spec-approval", "Spec approval", "A person approves spec.md before the build starts."]].map(
    ([id, label, rule]) => Object.freeze({ id, label, rule }),
  ),
);

const INVISIBLE =
  // oxlint-disable-next-line no-control-regex -- this pattern exists to reject control characters.
  /[\u0000-\u0009\u000b-\u001f\u007f-\u009f\u00ad\u115f\u1160\u180e\u200b-\u200f\u2028\u2029\u202a-\u202e\u2060-\u2064\u2066-\u2069\u3164\ufeff\uffa0\ufff9-\ufffb]|[\ufe00-\ufe0f]|[\u{e0000}-\u{e007f}]|[\u{e0100}-\u{e01ef}]/u;
// Links are the easiest exfiltration and fetch lure, with or without a scheme.
const LINKS = [/:\/\//, /\bwww\./i, /\b[a-z0-9-]+\.[a-z]{2,}\//i, /\b\d{1,3}(?:\.\d{1,3}){3}[:/]/];
const SECRETS = [
  /\b(?:gh[pousr])_[A-Za-z0-9]{20,}/,
  /github_pat_[A-Za-z0-9_]{20,}/,
  /\bsk-[A-Za-z0-9_-]{20,}/,
  /\bAKIA[0-9A-Z]{16}\b/,
  /-----BEGIN [A-Z ]*PRIVATE KEY-----/,
  /\bxox[abposr]-[A-Za-z0-9-]{10,}/,
  /(?<![A-Za-z0-9_-])eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\./,
  /cf-access-client-secret/i,
];
// A tripwire, not the control: false positives are accepted and the operator rewrites.
const PHRASES = [
  [
    "override",
    /\b(?:ignore|disregard|forget|override)\b[^.\n]{0,40}\b(?:instruction|rule|prompt|polic|guideline|previous|above|prior)/i,
  ],
  ["role", /\b(?:you are now|act as|new role|system prompt|developer message)\b/i],
  [
    "floor",
    /force[- ]?push|push (?:directly )?to (?:main|master|the base)|--no-verify|skip (?:the )?(?:review|approval|approver|tests?|checks?)|auto-?approve|merge without/i,
  ],
  [
    "exfil",
    /exfiltrat|send (?:the )?(?:token|secret|credential|\.env)|print (?:the )?(?:token|secret|env)/i,
  ],
  ["shell", /(?:curl|wget)[^\n]*\|\s*(?:sh|bash)|\brm -rf\b/i],
];

// Rule 4 folds look-alikes first. NFKC handles fullwidth and other compatibility forms. This table handles the brackets, slashes, dashes and Cyrillic, Greek and Latin letters NFKC leaves alone. factory.py holds the same two strings.
const TAG_LOOKALIKES_FROM =
  "\u2039\u27e8\u3008\u276e\u02c2\u2044\u2215\u29f8\u2571\u2010\u2011\u2012\u2013\u2014\u2015\u2212\u2043\u0430\u0435\u043e\u0440\u0441\u0455\u0410\u0415\u041e\u0420\u0421\u0405\u0422\u03b1\u03bf\u03c1\u0391\u0395\u039f\u03a1\u03a4\u0412\u0392\u041a\u039a\u043a\u03ba\u04c0\u04cf\u0261\u050d\u0456\u0406\u0399\u03b9\u0131\u041c\u039c\u043c";
const TAG_LOOKALIKES_TO = "<<<<<////--------aeopcsAEOPCSTaopAEOPTBBKKkkllggiIIiiMMm";
const TAG_LOOKALIKES = new Map([...TAG_LOOKALIKES_FROM].map((c, i) => [c, TAG_LOOKALIKES_TO[i]]));
const TAG_LOOKALIKE_CHARS = new RegExp(`[${TAG_LOOKALIKES_FROM}]`, "g");
const RESERVED_TAG_PATTERNS = new Map();
const SECRETS_ALL = SECRETS.map((p) => new RegExp(p.source, p.flags + "g"));

/** NFKC plus the look-alike map, so tag look-alikes read as ASCII. */
export function foldLookalikes(text) {
  return text.normalize("NFKC").replace(TAG_LOOKALIKE_CHARS, (c) => TAG_LOOKALIKES.get(c));
}

/** Text with every secret-shaped match replaced by `mark`. */
export function redactSecrets(text, mark) {
  return SECRETS_ALL.reduce((out, pattern) => out.replace(pattern, mark), text);
}

/** Whether text holds a secret shape. */
export function hasSecret(text) {
  return SECRETS.some((pattern) => pattern.test(text));
}

/** The regex source for a `name` tag after the fold, with i and l matching each other. */
export function foldedTagSource(name) {
  return `<\\/?\\s*${name.replace(/[il]/g, "[il]")}`;
}

/** The `{ tag, folded }` regexes for a reserved tag name, built once per name. */
function reservedTagPatterns(name) {
  let patterns = RESERVED_TAG_PATTERNS.get(name);
  if (!patterns) {
    patterns = {
      tag: new RegExp(`<\\/?\\s*${name}`, "i"),
      folded: new RegExp(foldedTagSource(name), "i"),
    };
    RESERVED_TAG_PATTERNS.set(name, patterns);
  }
  return patterns;
}

/**
 * Whether text holds a `name` tag as written, or once NFKC and the look-alike fold apply. The
 * folded check lets i and l match each other, since the palochka and Ukrainian I read as either.
 */
export function hasReservedTag(text, name) {
  const { tag, folded } = reservedTagPatterns(name);
  if (tag.test(text)) return true;
  // ASCII text is unchanged by the fold, so skip it.
  if (!/[\u0080-\uffff]/.test(text)) return false;
  return folded.test(foldLookalikes(text));
}

/**
 * `<field>: <reason>` for text an operator may not store, or null. `kind` "note" applies every
 * rule; "document" (project files) skips the length, link and phrase rules and allows tab and CR.
 */
export function checkOperatorText(text, field, kind = "note") {
  const note = kind !== "document";
  if (typeof text !== "string") return `${field}: must be text`;
  if (note && [...text].length > 2000) return `${field}: over 2000 characters`;
  if (INVISIBLE.test(note ? text : text.replace(/[\t\r]/g, "")))
    return `${field}: contains control, invisible or bidirectional characters`;
  if (hasReservedTag(text, "operator-preferences"))
    return `${field}: contains the reserved envelope tag`;
  if (note && LINKS.some((pattern) => pattern.test(text))) return `${field}: links are not allowed`;
  if (hasSecret(text)) return `${field}: looks like a secret`;
  if (!note) return null;
  for (const [id, pattern] of PHRASES) {
    if (pattern.test(text))
      return `${field}: reads like an instruction to override factory rules (${id})`;
  }
  return null;
}

const isObject = (value) => typeof value === "object" && value !== null && !Array.isArray(value);

/** The first schema error in a parsed pipeline document, or null. */
export function validatePipeline(doc) {
  if (!isObject(doc)) return "body: must be a JSON object";
  for (const key of Object.keys(doc)) if (!KEYS.includes(key)) return `${key}: unknown field`;
  if (doc.schema !== 1) return "schema: must be 1";
  if ("merge" in doc && doc.merge !== "auto" && doc.merge !== "human")
    return 'merge: must be "auto" or "human"';
  if (
    "review_rounds" in doc &&
    !(Number.isInteger(doc.review_rounds) && doc.review_rounds >= 1 && doc.review_rounds <= 5)
  )
    return "review_rounds: must be an integer from 1 to 5";
  if ("gates" in doc) {
    if (!Array.isArray(doc.gates)) return "gates: must be a list";
    for (const [i, id] of doc.gates.entries()) {
      if (!GATES.some((gate) => gate.id === id)) return `gates[${i}]: not a catalog gate`;
      if (doc.gates.indexOf(id) !== i) return `gates[${i}]: listed twice`;
    }
  }
  if ("pickup" in doc) {
    const pickup = doc.pickup;
    if (!isObject(pickup)) return "pickup: must be an object";
    for (const key of Object.keys(pickup))
      if (key !== "enabled" && key !== "hosts") return `pickup.${key}: unknown field`;
    if (typeof pickup.enabled !== "boolean") return "pickup.enabled: must be true or false";
    if (!Array.isArray(pickup.hosts)) return "pickup.hosts: must be a list";
    if (pickup.hosts.length > 32) return "pickup.hosts: at most 32 hosts";
    for (const [i, host] of pickup.hosts.entries()) {
      if (typeof host !== "string" || !HOST_NAME.test(host))
        return `pickup.hosts[${i}]: must be a host name of letters, digits, dot, dash and underscore`;
      if (pickup.hosts.indexOf(host) !== i) return `pickup.hosts[${i}]: listed twice`;
    }
  }
  if ("triage" in doc) {
    const triage = doc.triage;
    if (!isObject(triage)) return "triage: must be an object";
    for (const key of Object.keys(triage))
      if (!["enabled", "threshold", "rules"].includes(key)) return `triage.${key}: unknown field`;
    if (typeof triage.enabled !== "boolean") return "triage.enabled: must be true or false";
    if (!(Number.isInteger(triage.threshold) && triage.threshold >= 1 && triage.threshold <= 100))
      return "triage.threshold: must be an integer from 1 to 100";
    if (typeof triage.rules !== "string") return "triage.rules: must be text";
    if (triage.rules) {
      const error = checkOperatorText(triage.rules, "triage.rules");
      if (error) return error;
    }
  }
  if ("stage_notes" in doc) {
    if (!isObject(doc.stage_notes)) return "stage_notes: must be an object";
    for (const key of Object.keys(doc.stage_notes))
      if (!STAGES.includes(key)) return `stage_notes.${key}: unknown field`;
    for (const stage of STAGES) {
      if (!(stage in doc.stage_notes)) continue;
      const error = checkOperatorText(doc.stage_notes[stage], `stage_notes.${stage}`);
      if (error) return error;
    }
  }
  return "archive" in doc ? archiveError(doc.archive) : null;
}

/** The first error in a pipeline `archive` value, or null. The dashboard holds the same copy. */
function archiveError(archive) {
  if (!isObject(archive)) return "archive: must be an object";
  for (const key of Object.keys(archive))
    if (key !== "mode" && key !== "n") return `archive.${key}: unknown field`;
  if (archive.mode !== "age" && archive.mode !== "count")
    return 'archive.mode: must be "age" or "count"';
  if (!(Number.isInteger(archive.n) && archive.n >= 1 && archive.n <= 10000))
    return "archive.n: must be a whole number from 1 to 10000";
  return null;
}

const pickupOf = (p, activeHosts) =>
  p
    ? { enabled: p.enabled, hosts: p.hosts.filter((host) => !activeHosts || activeHosts.has(host)) }
    : { enabled: false, hosts: [] };

/**
 * The merge of two validated documents (or null) over the floors. `activeHosts` is
 * the Set of active registered host ids, or null before any host is registered.
 */
export function effectivePipeline(
  slug,
  global,
  globalVersion,
  project,
  projectVersion,
  activeHosts = null,
) {
  const scopes = [
    [global, `pipeline:global@${globalVersion}`],
    [project, `pipeline:${slug}@${projectVersion}`],
  ];
  const stageNotes = {};
  for (const stage of STAGES) {
    const notes = scopes
      .filter(([doc]) => doc?.stage_notes?.[stage] !== undefined)
      .map(([doc, source]) => ({ source, text: doc.stage_notes[stage] }));
    if (notes.length) stageNotes[stage] = notes;
  }
  return {
    project: slug,
    merge: global?.merge === "human" || project?.merge === "human" ? "human" : "auto",
    review_rounds: Math.min(
      5,
      global?.review_rounds ?? 2,
      project?.review_rounds ?? global?.review_rounds ?? 2,
    ),
    reviewers: [...REVIEWERS],
    gates: GATES.map((gate) => gate.id).filter(
      (id) => global?.gates?.includes(id) || project?.gates?.includes(id),
    ),
    pickup: pickupOf(project?.pickup ?? global?.pickup, activeHosts),
    triage: project?.triage ?? global?.triage ?? { enabled: false, threshold: 90, rules: "" },
    stage_notes: stageNotes,
    versions: { global: globalVersion, project: projectVersion },
    floors: FLOORS.map((floor) => floor.id),
  };
}
