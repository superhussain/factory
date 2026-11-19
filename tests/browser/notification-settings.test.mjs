// Notifications section and the Display card details and New idea project additions,
// against one owned local Worker. Stub Notification and AudioContext classes stand in for
// the browser's.
import assert from "node:assert/strict";
import test from "node:test";
import { prepareAssets } from "../../tools/assets.mjs";
import {
  blockLiveEvents,
  withBrowserFixture,
  seedProject,
  seedBoard,
} from "../support/browser-server.mjs";
import projectFilter from "../support/project-filter.cjs";

const { showOnly } = projectFilter;
const NOTE = { branch: "factory/001-sample", round: 1, note: "Synthetic note" };
const JOBS = [
  { id: "001-sample", project: "sample", title: "Synthetic sample job", stage: "spec", ...NOTE },
  {
    id: "004-sample-pr",
    project: "sample",
    title: "Synthetic PR job",
    stage: "pr-open",
    pr: "https://github.com/example/sample/pull/7",
    ...NOTE,
  },
  {
    id: "005-sample-needs",
    project: "sample",
    title: "Synthetic needs job",
    stage: "build",
    needs: "Approve the spec",
  },
].map((job) => ({ ...job, updated: "2026-01-01T00:00:00Z" }));
const wait = { polling: 50 };
// A synthetic VAPID public key: 0x04 and 64 bytes.
const PUSH_KEY = Buffer.concat([Buffer.from([4]), Buffer.alloc(64, 7)]).toString("base64url");
const ENDPOINT = "https://fcm.googleapis.com/fcm/send/sample";
const IDEA = "- [ ] sample: Synthetic backlog idea\n";
const SOUND_LABELS = [
  ["backlog", "A new backlog item", "Someone adds an idea to the backlog."],
  ["needs", "A job needs you", "A job stops and waits on your input."],
  ["spec", "A spec is ready", "A job moves on from its spec stage."],
  ["pr", "A pull request opens", "A job opens its pull request for review."],
  ["merged", "A job merges", "A job's pull request merges."],
];
const SOUND_HELP =
  "Pick which events play a sound in the Events list below. Sounds play in this tab, visible or not, after you click or type in it once since it loaded. This setting applies only to this browser.";
const SOUND_OFF_HELP =
  "Sounds are off, so the Sound boxes in the Events list below are greyed out. Turn this on to pick which events play a sound. Play still previews each sound.";
const PUSH_HELP =
  "Sends a push when a job starts waiting on you or merges. It shows the job id and title only.";

async function seedBacklog(origin, text) {
  const current = await fetch(origin + "/api/backlog");
  const condition = current.ok
    ? { "If-Match": current.headers.get("ETag") }
    : { "If-None-Match": "*" };
  await current.text();
  const response = await fetch(origin + "/api/backlog", {
    method: "PUT",
    headers: { "Content-Type": "text/markdown", ...condition },
    body: text,
  });
  assert.equal(response.status, 200);
}

test("notification settings", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  await withBrowserFixture(async ({ browser, origin }) => {
    await seedProject(
      origin,
      "sample",
      "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n",
    );
    await seedProject(
      origin,
      "other",
      "---\nrepo: example/other\nbase: main\ntitle: Synthetic other\n---\n",
    );
    await seedBoard(origin, JOBS);
    const context = await browser.createBrowserContext();
    const page = await context.newPage();
    await page.setViewport({ width: 1280, height: 900 });
    const errors = [];
    page.on("pageerror", (error) => errors.push(String(error)));
    await blockLiveEvents(page);
    // Board events here would raise toasts over the controls; notify-toast.test.mjs covers them.
    await page.evaluateOnNewDocument(() => {
      document.addEventListener("DOMContentLoaded", () => {
        document.getElementById("notify-toasts").style.display = "none";
      });
    });
    await page.evaluateOnNewDocument(() => {
      const interval = window.setInterval;
      const clear = window.clearInterval;
      window.pollTimers = new Map();
      window.setInterval = (fn, ms, ...args) => {
        const id = interval(fn, ms, ...args);
        if (fn.name === "pollTick") window.pollTimers.set(id, ms);
        return id;
      };
      window.clearInterval = (id) => {
        window.pollTimers.delete(id);
        return clear(id);
      };
    });
    await page.evaluateOnNewDocument(() => {
      if (sessionStorage.getItem("no-notify")) {
        delete window.Notification;
        return;
      }
      window.shown = [];
      window.Notification = class {
        static permission = "default";
        static async requestPermission() {
          this.permission = window.__grant;
          return this.permission;
        }
        constructor(title, { body, tag }) {
          window.shown.push({ title, body, tag, instance: this });
        }
        close() {}
      };
    });
    await page.evaluateOnNewDocument(() => {
      if (sessionStorage.getItem("no-audio")) {
        delete window.AudioContext;
        return;
      }
      window.tones = [];
      window.toneStarts = [];
      const param = () => ({ value: 0, setValueAtTime() {}, exponentialRampToValueAtTime() {} });
      window.AudioContext = class {
        state = "suspended";
        currentTime = 0;
        destination = {};
        async resume() {
          this.state = "running";
        }
        createGain() {
          return { gain: param(), connect: (node) => node };
        }
        createOscillator() {
          const osc = {
            frequency: param(),
            connect: (node) => node,
            start: (at) => {
              window.tones.push(osc.frequency.value);
              window.toneStarts.push(at);
            },
            stop() {},
          };
          return osc;
        }
      };
    });

    // The Worker's push routes, answered here: keyStatus for the key, subStatus for writes.
    const push = { keyStatus: 200, subStatus: 204, requests: [] };
    await page.setRequestInterception(true);
    page.on("request", (request) => {
      const path = new URL(request.url()).pathname;
      if (path === "/api/push/key")
        return request.respond(
          push.keyStatus === 200
            ? {
                status: 200,
                contentType: "application/json",
                body: JSON.stringify({ key: PUSH_KEY }),
              }
            : { status: push.keyStatus, body: "not found" },
        );
      if (path === "/api/push/subscriptions") {
        push.requests.push([request.method(), JSON.parse(request.postData())]);
        if (push.subStatus === "abort") return request.abort();
        return request.respond({ status: push.subStatus, body: "" });
      }
      return request.continue();
    });
    await page.evaluateOnNewDocument((endpoint) => {
      window.pushCalls = [];
      window.PushManager = class {};
      let current = null;
      const pushManager = {
        getSubscription: async () => current,
        subscribe: async ({ userVisibleOnly, applicationServerKey }) => {
          window.pushCalls.push(["subscribe", userVisibleOnly, [...applicationServerKey]]);
          current = {
            endpoint,
            toJSON: () => ({ endpoint, expirationTime: null, keys: { p256dh: "p", auth: "a" } }),
            unsubscribe: async () => {
              if (window.unsubscribeFails) throw new Error("synthetic failure");
              window.pushCalls.push(["unsubscribe"]);
              current = null;
              return true;
            },
          };
          return current;
        },
      };
      Object.defineProperty(navigator, "serviceWorker", {
        configurable: true,
        value: { register: async () => ({}), ready: Promise.resolve({ pushManager }) },
      });
    }, ENDPOINT);

    const liveTimers = () => page.evaluate(() => [...window.pollTimers.values()]);
    const setHidden = (hidden) =>
      page.evaluate((value) => {
        Object.defineProperty(document, "hidden", { configurable: true, get: () => value });
        document.dispatchEvent(new Event("visibilitychange"));
      }, hidden);
    const storage = (key) => page.evaluate((k) => localStorage.getItem(k), key);
    const setStorage = (key, value) =>
      page.evaluate((k, v) => localStorage.setItem(k, v), key, value);
    const text = (selector) => page.$eval(selector, (el) => el.textContent);
    const toHash = async (hash, selector) => {
      await page.evaluate((h) => {
        location.hash = h;
      }, hash);
      await page.waitForSelector(selector, wait);
    };
    const reload = async (selector) => {
      await page.reload({ waitUntil: "domcontentloaded" });
      await page.waitForSelector(selector, wait);
      await page.waitForFunction(() => window.pollTimers.size > 0, wait);
    };
    // The Display controls sit below the fold. Scroll, hit-test and press in one page task,
    // so a late layout shift cannot separate them and a covered control still fails.
    const pressBelowFold = async (selector) => {
      const covering = await page.$eval(selector, (el) => {
        el.scrollIntoView({ block: "center", behavior: "instant" });
        const r = el.getBoundingClientRect();
        const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
        if (hit !== el && !el.contains(hit)) return hit?.id || hit?.tagName || "nothing";
        el.click();
        return null;
      });
      assert.equal(covering, null, `${selector} is covered by ${covering}`);
    };
    const tones = () => page.evaluate(() => window.tones);
    const soundBoxes = () =>
      page.$$eval("[data-sound-kind]", (els) => els.map((el) => [el.checked, el.disabled]));
    const shown = () =>
      page.evaluate(() => window.shown.map(({ title, body, tag }) => ({ title, body, tag })));

    await page.goto(origin + "/#/settings/notifications", { waitUntil: "domcontentloaded" });
    await page.waitForSelector("#settings-notifications:not([hidden])", wait);
    await page.waitForFunction(() => window.pollTimers.size > 0, wait);

    await t.test("notify N1 route, nav and focus", async () => {
      assert.equal(await page.$eval("#settings-display", (el) => el.hidden), true);
      assert.equal(
        await page.$eval("#settings-nav-notifications", (el) => el.getAttribute("aria-current")),
        "page",
      );
      assert.equal(
        await page.$eval("#settings-nav-display", (el) => el.getAttribute("aria-current")),
        null,
      );
      await page.waitForFunction(() => document.activeElement?.id === "notify-needs", wait);
    });

    await t.test("notify N2 denied and granted prompts", async () => {
      await page.evaluate(() => {
        window.__grant = "denied";
      });
      await page.click("#notify-needs");
      await page.waitForFunction(
        () => document.getElementById("notify-status").textContent === "Notifications are off.",
        wait,
      );
      assert.equal(await page.$eval("#notify-needs", (el) => el.checked), false);
      assert.equal(await storage("factory-notify"), null);
      assert.equal(
        await text("#notify-permission"),
        "Notifications are blocked for this site. Allow them in your browser's site settings, then turn this on again.",
      );
      await page.evaluate(() => {
        window.Notification.permission = "default";
        window.__grant = "granted";
      });
      await page.click("#notify-needs");
      await page.waitForFunction(
        () =>
          document.getElementById("notify-status").textContent ===
          "Notifications are on in this browser.",
        wait,
      );
      assert.equal(await storage("factory-notify"), "on");
      assert.equal(await page.$eval("#notify-needs", (el) => el.checked), true);
      assert.equal(await text("#notify-permission"), "Browser permission: allowed.");
    });

    await t.test("notify N3 hidden tab polls only while notifications are on", async () => {
      await setHidden(true);
      assert.deepEqual(await liveTimers(), [30000]);
      await page.click("#notify-needs");
      await page.waitForFunction(
        () => document.getElementById("notify-status").textContent === "Notifications are off.",
        wait,
      );
      assert.deepEqual(await liveTimers(), []);
      await page.click("#notify-needs");
      await page.waitForFunction(
        () =>
          document.getElementById("notify-status").textContent ===
          "Notifications are on in this browser.",
        wait,
      );
      assert.deepEqual(await liveTimers(), [30000]);
      await setHidden(false);
      await page.waitForFunction(() => [...window.pollTimers.values()].includes(30000), wait);
    });

    await t.test("notify N4 one notification per newly waiting job", async () => {
      const pick = JOBS.map((j) => (j.id === "001-sample" ? { ...j, needs: "Pick one" } : j));
      await seedBoard(origin, pick);
      await page.evaluate(() => poll());
      assert.deepEqual(await shown(), []);
      await setHidden(true);
      await seedBoard(origin, [
        ...pick,
        {
          id: "006-sample-wait",
          project: "sample",
          title: "Synthetic waiting job",
          stage: "needs-human",
          updated: "2026-01-01T00:00:00Z",
        },
      ]);
      await page.evaluate(() => poll());
      await page.evaluate(() => poll());
      assert.deepEqual(await shown(), [
        {
          title: "Needs you: 006-sample-wait",
          body: "Synthetic waiting job",
          tag: "factory-needs-006-sample-wait",
        },
      ]);
      await page.evaluate(() => window.shown[0].instance.onclick());
      assert.equal(await page.evaluate(() => location.hash), "#006-sample-wait");
      await setHidden(false);
      await toHash("#/settings/display", "#settings-display:not([hidden])");
    });

    await t.test("notify P1 push toggle subscribes and unsubscribes", async () => {
      const pushText = (help, status) =>
        page.waitForFunction(
          (h, st) =>
            document.getElementById("push-help").textContent === h &&
            document.getElementById("push-status").textContent === st,
          wait,
          help,
          status,
        );
      const pushCalls = () => page.evaluate(() => window.pushCalls.splice(0));
      await toHash("#/settings/notifications", "#settings-notifications:not([hidden])");
      await page.waitForSelector("#notify-push:not([disabled])", wait);
      assert.equal(
        await text('label[for="notify-push"]'),
        " Push to this device when the dashboard is closed",
      );
      assert.equal(await text("#push-help"), PUSH_HELP);
      assert.equal(await page.$eval("#notify-push", (el) => el.checked), false);

      await page.click("#notify-push");
      await pushText(PUSH_HELP, "Push is on for this device.");
      assert.equal(await page.$eval("#notify-push", (el) => el.checked), true);
      assert.deepEqual(await pushCalls(), [
        ["subscribe", true, [...Buffer.from(PUSH_KEY, "base64url")]],
      ]);
      assert.deepEqual(push.requests.splice(0), [
        ["PUT", { endpoint: ENDPOINT, expirationTime: null, keys: { p256dh: "p", auth: "a" } }],
      ]);

      await page.click("#notify-push");
      await pushText(PUSH_HELP, "Push is off for this device.");
      assert.equal(await page.$eval("#notify-push", (el) => el.checked), false);
      assert.deepEqual(await pushCalls(), [["unsubscribe"]]);
      assert.deepEqual(push.requests.splice(0), [["DELETE", { endpoint: ENDPOINT }]]);

      // "abort" makes the PUT throw after a good subscribe; the local subscription must go too.
      for (const [status, line] of [
        [409, "This instance already has 20 push devices. Turn push off on one first."],
        [500, "Push could not be turned on. Try again."],
        ["abort", "Push could not be turned on. Try again."],
      ]) {
        push.subStatus = status;
        await page.click("#notify-push");
        await pushText(PUSH_HELP, line);
        // The abort line repeats the 500 line, so also wait for setPush to finish.
        await page.waitForSelector("#notify-push:not([disabled])", wait);
        assert.equal(await page.$eval("#notify-push", (el) => el.checked), false);
        assert.deepEqual(
          (await pushCalls()).map(([name]) => name),
          ["subscribe", "unsubscribe"],
        );
        assert.equal(push.requests.splice(0).length, 1);
      }
      push.subStatus = 204;

      await page.click("#notify-push");
      await pushText(PUSH_HELP, "Push is on for this device.");
      await page.evaluate(() => {
        window.unsubscribeFails = true;
      });
      await page.click("#notify-push");
      await page.waitForSelector("#notify-push:not([disabled])", wait);
      assert.equal(await page.$eval("#notify-push", (el) => el.checked), true);
      assert.equal(await text("#push-status"), "Push is on for this device.");
      // Unsubscribe runs before the DELETE, so a failed unsubscribe keeps the server record.
      assert.deepEqual(push.requests.splice(0), [
        ["PUT", { endpoint: ENDPOINT, expirationTime: null, keys: { p256dh: "p", auth: "a" } }],
      ]);
      await page.evaluate(() => {
        window.unsubscribeFails = false;
      });
      await page.click("#notify-push");
      await pushText(PUSH_HELP, "Push is off for this device.");
      await pushCalls();
      push.requests.splice(0);

      const reenter = async () => {
        await toHash("#/settings/display", "#settings-display:not([hidden])");
        await toHash("#/settings/notifications", "#settings-notifications:not([hidden])");
      };
      push.keyStatus = 404;
      await reenter();
      await page.waitForFunction(
        () =>
          document.getElementById("push-help").textContent ===
          "Push is not set up on this instance. See docs/cloudflare.md.",
        wait,
      );
      assert.equal(await page.$eval("#notify-push", (el) => el.disabled), true);
      push.keyStatus = 500;
      await reenter();
      await page.waitForFunction(
        () =>
          document.getElementById("push-help").textContent ===
          "Push status could not be loaded. Check your connection or sign in again, then reopen this page.",
        wait,
      );
      assert.equal(await page.$eval("#notify-push", (el) => el.disabled), true);
      push.keyStatus = 200;

      await page.evaluate(() => {
        window.Notification.permission = "denied";
      });
      await reenter();
      await page.waitForFunction(
        (denied) => document.getElementById("push-help").textContent === denied,
        wait,
        "Notifications are blocked for this site. Allow them in your browser's site settings, then turn this on again.",
      );
      await page.click("#notify-push");
      await page.waitForFunction(
        () => document.getElementById("push-status").textContent === "Push is off for this device.",
        wait,
      );
      assert.deepEqual(await pushCalls(), []);
      await page.evaluate(() => {
        window.Notification.permission = "granted";
      });
      await toHash("#/settings/display", "#settings-display:not([hidden])");
    });

    await t.test("notify N6 reminder setting", async () => {
      await toHash("#/settings/notifications", "#settings-notifications:not([hidden])");
      assert.deepEqual(
        await page.$$eval("#notify-remind option", (els) =>
          els.map((el) => [el.value, el.textContent]),
        ),
        [
          ["", "Never"],
          ["1", "1 hour"],
          ["4", "4 hours"],
          ["24", "1 day"],
        ],
      );
      assert.equal(await page.$eval("#notify-remind", (el) => el.value), "");
      assert.equal(await page.$eval("#notify-remind", (el) => el.disabled), false);
      await page.click("#notify-needs");
      await page.waitForFunction(() => document.getElementById("notify-remind").disabled, wait);
      await page.click("#notify-needs");
      await page.waitForFunction(() => !document.getElementById("notify-remind").disabled, wait);
      await page.select("#notify-remind", "4");
      assert.equal(await storage("factory-notify-remind"), "4");
      assert.equal(await text("#notify-status"), "Reminders are on: after 4 hours.");
      await page.select("#notify-remind", "");
      assert.equal(await storage("factory-notify-remind"), null);
      assert.equal(await text("#notify-status"), "Reminders are off.");
    });

    await t.test("notify N7 reminder fires once per wait", async () => {
      await toHash("#/settings/notifications", "#settings-notifications:not([hidden])");
      await page.select("#notify-remind", "1");
      const fresh = new Date(Date.now() - 30 * 60000).toISOString();
      const waiting = (needs, since) => [
        ...JOBS.filter((j) => j.id !== "005-sample-needs"),
        { ...JOBS[2], updated: needs },
        {
          id: "006-sample-wait",
          project: "sample",
          title: "Synthetic waiting job",
          stage: "needs-human",
          updated: since,
        },
      ];
      const remind = async () => {
        await page.evaluate(() => {
          window.shown = [];
        });
        await page.evaluate(() => poll());
        await setHidden(true);
        await page.evaluate(() => remindWaiting());
        return shown();
      };
      await seedBoard(origin, waiting("2026-01-01T00:00:00Z", fresh));
      assert.deepEqual(await remind(), [
        {
          title: "Still needs you: 005-sample-needs",
          body: "Synthetic needs job",
          tag: "factory-remind",
        },
      ]);
      await page.evaluate(() => remindWaiting());
      assert.equal((await shown()).length, 1);
      await setHidden(false);
      await seedBoard(origin, waiting("2026-01-01T01:00:00Z", "2026-01-01T00:00:00Z"));
      assert.deepEqual(await remind(), [
        {
          title: "Still needs you: 2 jobs",
          body: "006-sample-wait, 005-sample-needs",
          tag: "factory-remind",
        },
      ]);
      await setHidden(false);
      await seedBoard(origin, waiting("2026-01-01T02:00:00Z", fresh));
      await page.evaluate(() => {
        window.shown = [];
      });
      await page.evaluate(() => poll());
      await page.evaluate(() => remindWaiting());
      assert.deepEqual(await shown(), []);
      await page.select("#notify-remind", "");
      await setHidden(true);
      await page.evaluate(() => remindWaiting());
      assert.deepEqual(await shown(), []);
      await setHidden(false);
      await page.select("#notify-remind", "1");
      await toHash("#/settings/display", "#settings-display:not([hidden])");
    });

    await t.test("sound S1 block copy, off by default, and Play buttons", async () => {
      await toHash("#/settings/notifications", "#settings-notifications:not([hidden])");
      assert.equal(await text("#sound-heading"), "Sounds");
      assert.equal(await text('label[for="sound-on"]'), " Play a sound for board events");
      assert.ok(
        await page.evaluate(
          () =>
            document
              .getElementById("sound-heading")
              .compareDocumentPosition(document.getElementById("alert-heading")) &
            Node.DOCUMENT_POSITION_FOLLOWING,
        ),
      );
      assert.equal(await text("#sound-help"), SOUND_OFF_HELP);
      assert.equal(await page.$eval("#sound-on", (el) => el.checked), false);
      assert.deepEqual(await soundBoxes(), Array(5).fill([true, true]));
      assert.deepEqual(
        await page.$$eval("[data-sound]", (els) => els.map((el) => el.disabled)),
        Array(5).fill(false),
      );
      assert.equal(await storage("factory-sound"), null);
      assert.deepEqual(
        await page.$$eval(".event-kinds li", (els) =>
          els.map((li) => {
            const box = li.querySelector("input[data-sound-kind]");
            return [
              li.querySelector("span").firstChild.textContent,
              li.querySelector(".event-desc").textContent,
              box.id,
              box.dataset.soundKind,
              box.checked,
              box.getAttribute("aria-label"),
              box.closest("label").textContent,
              li.querySelector("button").textContent,
              li.querySelector("button").getAttribute("aria-label"),
            ];
          }),
        ),
        SOUND_LABELS.map(([kind, label, desc]) => [
          label,
          desc,
          "sound-" + kind,
          kind,
          true,
          "Sound for " + label,
          " Sound",
          "Play",
          "Play the sound for: " + label,
        ]),
      );
      for (const [kind, expected] of [
        ["backlog", [392, 494]],
        ["spec", [659, 880]],
        ["pr", [523, 659]],
        ["needs", [880, 660, 880]],
        ["merged", [523, 659, 784]],
      ]) {
        await page.evaluate(() => {
          window.tones = [];
        });
        await page.click(`[data-sound="${kind}"]`);
        assert.deepEqual(await tones(), expected);
      }
      assert.equal(await page.evaluate(() => audio.state), "running");
    });

    await t.test("sound S2 checkbox stores the setting", async () => {
      await page.click("#sound-on");
      await page.waitForFunction(
        () =>
          document.getElementById("sound-status").textContent === "Sounds are on in this browser.",
        wait,
      );
      assert.equal(await storage("factory-sound"), "on");
      assert.deepEqual(await soundBoxes(), Array(5).fill([true, false]));
      assert.equal(await text("#sound-help"), SOUND_HELP);
      await page.click("#sound-on");
      await page.waitForFunction(
        () => document.getElementById("sound-status").textContent === "Sounds are off.",
        wait,
      );
      assert.equal(await storage("factory-sound"), null);
      assert.deepEqual(await soundBoxes(), Array(5).fill([true, true]));
      assert.equal(await text("#sound-help"), SOUND_OFF_HELP);
    });

    await t.test("sound S3 hidden tab polls while sounds are on", async () => {
      await page.click("#notify-needs");
      await page.waitForFunction(
        () => document.getElementById("notify-status").textContent === "Notifications are off.",
        wait,
      );
      await setHidden(true);
      assert.deepEqual(await liveTimers(), []);
      await page.click("#sound-on");
      assert.deepEqual(await liveTimers(), [30000]);
      await page.click("#sound-on");
      assert.deepEqual(await liveTimers(), []);
      await page.click("#sound-on");
      await page.click("#notify-needs");
      await page.waitForFunction(
        () =>
          document.getElementById("notify-status").textContent ===
          "Notifications are on in this browser.",
        wait,
      );
      await setHidden(false);
    });

    await t.test("sound S4 one tone per board read", async () => {
      await seedBoard(origin, JOBS);
      await reload("#settings-notifications:not([hidden])");
      await page.waitForFunction(() => soundSeen !== null, wait);
      await page.keyboard.press("Shift");
      await page.waitForFunction(() => audio?.state === "running", wait);
      assert.equal(await page.$eval("#sound-on", (el) => el.checked), true);
      assert.deepEqual(await tones(), []);
      const moved = JOBS.map((j) =>
        j.id === "001-sample"
          ? { ...j, stage: "pr-open" }
          : j.id === "004-sample-pr"
            ? { ...j, stage: "merged" }
            : j,
      );
      await seedBoard(origin, [
        ...moved,
        {
          id: "006-sample-wait",
          project: "sample",
          title: "Synthetic waiting job",
          stage: "needs-human",
          updated: "2026-01-01T00:00:00Z",
        },
      ]);
      await page.evaluate(() => poll());
      assert.deepEqual(await tones(), [880, 660, 880]);
      await seedBoard(
        origin,
        moved.map((j) => (j.id === "001-sample" ? { ...j, stage: "merged" } : j)),
      );
      await page.evaluate(() => poll());
      assert.deepEqual(await tones(), [880, 660, 880, 523, 659, 784]);
      await page.evaluate(() => poll());
      assert.deepEqual(await tones(), [880, 660, 880, 523, 659, 784]);
      await toHash("#/settings/display", "#settings-display:not([hidden])");
    });

    let afterS5;
    await t.test("sound S5 sound and toast turn off on their own", async () => {
      // S4 left 001 and 004 merged. New jobs move, so the jobs C1 reads stay as they are.
      const board = JOBS.map((j) => (j.id === "005-sample-needs" ? j : { ...j, stage: "merged" }));
      const extra = (id, stage) => ({ ...JOBS[0], id, title: "Synthetic job " + id, stage });
      const toastKinds = (id) =>
        page.$$eval(
          "#notify-toasts a",
          (els, href) =>
            els
              .filter((a) => a.getAttribute("href") === href)
              .map((a) => a.querySelector(".inbox-kind").textContent),
          "#" + id,
        );
      await seedBoard(origin, [
        ...board,
        extra("007-sample-merge", "build"),
        extra("073-sample-retry", "needs-human"),
      ]);
      await page.evaluate(() => poll());
      await page.evaluate(() => {
        window.tones = [];
      });
      await toHash("#/settings/notifications", "#settings-notifications:not([hidden])");
      await page.click("#sound-pr");
      assert.equal(await storage("factory-sounds-off"), "pr");
      assert.equal(await text("#alert-status"), "Sound settings saved for this browser.");
      await seedBoard(origin, [
        ...board,
        extra("007-sample-merge", "build"),
        extra("073-sample-retry", "pr-open"),
      ]);
      await page.evaluate(() => poll());
      assert.deepEqual(await tones(), []);
      assert.deepEqual(await toastKinds("073-sample-retry"), ["PR open"]);
      await page.click("#alert-merged");
      assert.equal(await storage("factory-alerts-off"), "merged");
      assert.equal(await text("#alert-status"), "Toast settings saved for this browser.");
      afterS5 = [
        ...board,
        extra("007-sample-merge", "merged"),
        extra("073-sample-retry", "pr-open"),
      ];
      await seedBoard(origin, afterS5);
      await page.evaluate(() => poll());
      assert.deepEqual(await tones(), [523, 659, 784]);
      assert.deepEqual(await toastKinds("007-sample-merge"), []);
      await toHash("#/settings/display", "#settings-display:not([hidden])");
    });

    await t.test("sound S6 events block copy and stored toast kinds", async () => {
      const boxes = () =>
        page.$$eval(".event-kinds input[data-alert]", (els) =>
          els.map((el) => [
            el.id,
            el.dataset.alert,
            el.getAttribute("aria-label"),
            el.closest("label").textContent,
            el.checked,
          ]),
        );
      const expected = (merged) =>
        SOUND_LABELS.map(([kind, label]) => [
          "alert-" + kind,
          kind,
          "Toast for " + label,
          " Toast",
          kind === "merged" ? merged : true,
        ]);
      await toHash("#/settings/notifications", "#settings-notifications:not([hidden])");
      assert.equal(await text("#alert-heading"), "Events");
      assert.equal(
        await text("#alert-help"),
        "Pick, for each board event, whether it shows a toast in this tab and whether it plays a sound. These settings apply only to this browser.",
      );
      // Each event's name, Toast box, Sound box and Play button share one row.
      assert.deepEqual(
        await page.$$eval(".event-kinds li", (els) =>
          els.map((li) => {
            const mid = (el) => {
              const r = el.getBoundingClientRect();
              return Math.round(r.top + r.height / 2);
            };
            const parts = li.querySelectorAll("input, button");
            return [...parts].every((el) => Math.abs(mid(el) - mid(parts[0])) <= 2);
          }),
        ),
        [true, true, true, true, true],
      );
      assert.deepEqual(await boxes(), expected(false));
      await setStorage("factory-alerts-off", "bogus,merged");
      await reload("#settings-notifications:not([hidden])");
      assert.deepEqual(await boxes(), expected(false));
      assert.deepEqual(await page.evaluate(() => [...alertsOff]), ["merged"]);
      await toHash("#/settings/display", "#settings-display:not([hidden])");
    });

    await t.test("sound S8 spec ready and backlog tones", async () => {
      await page.waitForFunction(() => soundSeen !== null, wait);
      const spec = {
        ...JOBS[0],
        id: "009-sample-spec",
        title: "Synthetic spec job",
        stage: "spec",
      };
      await seedBoard(origin, [...afterS5, spec]);
      await page.evaluate(() => poll());
      await page.evaluate(() => {
        window.tones = [];
      });
      await seedBoard(origin, [...afterS5, { ...spec, stage: "build" }]);
      await page.evaluate(() => poll());
      assert.deepEqual(await tones(), [659, 880]);
      await seedBacklog(origin, "- [ ] sample: Synthetic existing idea\n");
      await page.evaluate(() => refreshBacklog());
      await page.evaluate(() => {
        window.tones = [];
      });
      await seedBacklog(origin, "- [ ] sample: Synthetic existing idea\n" + IDEA);
      await page.evaluate(() => refreshBacklog());
      assert.deepEqual(await tones(), [392, 494]);
    });

    await t.test("sound S9 a board tone and a backlog tone from one poll take turns", async () => {
      const both = {
        ...JOBS[0],
        id: "010-sample-both",
        title: "Synthetic both job",
        stage: "build",
      };
      const backlog = "- [ ] sample: Synthetic existing idea\n" + IDEA;
      await seedBoard(origin, [...afterS5, both]);
      await page.evaluate(() => poll());
      await page.evaluate(() => refreshBacklog());
      await page.evaluate(() => {
        window.tones = [];
        window.toneStarts = [];
        soundEnd = -Infinity;
      });
      await seedBoard(origin, [...afterS5, { ...both, stage: "needs-human" }]);
      await seedBacklog(origin, backlog + "- [ ] sample: Synthetic second idea\n");
      await page.evaluate(() => poll());
      await page.waitForFunction(() => window.tones.length === 5, wait);
      assert.deepEqual(
        (await tones()).sort((a, b) => a - b),
        [392, 494, 660, 880, 880],
      );
      const starts = (await page.evaluate(() => window.toneStarts)).sort((a, b) => a - b);
      for (let i = 1; i < starts.length; i++)
        assert.ok(starts[i] - starts[i - 1] >= 0.149, `${starts}`);
    });

    await t.test("notify C1 card detail toggles", async () => {
      for (const f of ["note", "meta", "pr"]) await page.click("#display-card-field-" + f);
      assert.equal(await storage("factory-hidden-card-fields"), "note,meta,pr");
      await page.click("#settings-back");
      await page.waitForSelector('.card[data-id="004-sample-pr"]', wait);
      const pr = '.card[data-id="004-sample-pr"]';
      assert.equal(await page.$(pr + " .note"), null);
      assert.equal(await page.$(pr + " .meta:not(.refs)"), null);
      assert.equal(await page.$("#board .pr-link"), null);
      assert.equal(
        await text('.card.needs-you[data-id="005-sample-needs"] .note'),
        "Approve the spec",
      );
      await setStorage("factory-hidden-card-fields", "note,bogus");
      await page.evaluate(() => {
        location.hash = "#/settings/display";
      });
      await reload("#settings-display:not([hidden])");
      assert.deepEqual(
        await page.$$eval("#display-card-fields input", (els) =>
          els.filter((el) => !el.checked).map((el) => el.dataset.field),
        ),
        ["note"],
      );
    });

    await t.test("notify C2 New idea project default", async () => {
      assert.deepEqual(
        await page.$$eval("#display-idea-project option", (els) => els.map((el) => el.value)),
        await page.$$eval("#idea-project option", (els) => els.map((el) => el.value)),
      );
      await page.select("#display-idea-project", "other");
      assert.equal(await storage("factory-idea-project"), "other");
      const ideaProject = async () => {
        await page.click("#new-idea");
        const value = await page.$eval("#idea-project", (el) => el.value);
        await page.evaluate(() => document.getElementById("idea-dlg").close());
        return value;
      };
      await showOnly(page, "", "#display-filter");
      assert.equal(await ideaProject(), "other");
      await showOnly(page, "sample", "#display-filter");
      assert.equal(await ideaProject(), "sample");
      await showOnly(page, "", "#display-filter");
      await setStorage("factory-idea-project", "ghost");
      await reload("#settings-display:not([hidden])");
      assert.equal(await page.$eval("#display-idea-project", (el) => el.value), "");
      assert.equal(await ideaProject(), "");
    });

    await t.test("notify C3 display reset keeps notifications", async () => {
      await page.select("#display-idea-project", "other");
      await pressBelowFold("#display-card-field-pr");
      assert.equal(await page.$eval("#display-card-field-pr", (el) => el.checked), false);
      await pressBelowFold("#display-reset");
      assert.equal(await storage("factory-hidden-card-fields"), null);
      assert.equal(await storage("factory-idea-project"), null);
      assert.equal(await storage("factory-notify"), "on");
      assert.equal(await storage("factory-sound"), "on");
      assert.equal(await storage("factory-notify-remind"), "1");
      assert.equal(await storage("factory-alerts-off"), "bogus,merged");
      assert.equal(await storage("factory-sounds-off"), "pr");
      assert.equal(
        await page.$$eval("#display-card-fields input", (els) => els.every((el) => el.checked)),
        true,
      );
    });

    await t.test("notify C4 mobile layout", async () => {
      await page.setViewport({ width: 390, height: 844 });
      for (const [hash, selector] of [
        ["#/settings/display", "#settings-display:not([hidden])"],
        ["#/settings/notifications", "#settings-notifications:not([hidden])"],
      ]) {
        await toHash(hash, selector);
        await page.waitForFunction(
          () => document.documentElement.scrollWidth <= window.innerWidth,
          wait,
        );
      }
      await page.setViewport({ width: 1280, height: 900 });
    });

    await t.test("notify N5 unsupported browser", async () => {
      await page.evaluate(() => sessionStorage.setItem("no-notify", "1"));
      await reload("#settings-notifications:not([hidden])");
      assert.equal(await page.evaluate(() => "Notification" in window), false);
      assert.equal(await page.$eval("#notify-needs", (el) => el.disabled), true);
      assert.equal(await page.$eval("#notify-needs", (el) => el.checked), false);
      assert.equal(
        await text("#notify-permission"),
        "This browser does not support notifications.",
      );
      assert.equal(await page.$eval("#notify-push", (el) => el.disabled), true);
      assert.equal(
        await text("#push-help"),
        "This browser cannot receive push. On iPhone or iPad, add the dashboard to your Home Screen and open it from there.",
      );
    });

    await t.test("sound S7 no Web Audio", async () => {
      await page.evaluate(() => sessionStorage.setItem("no-audio", "1"));
      await reload("#settings-notifications:not([hidden])");
      assert.equal(await page.evaluate(() => "AudioContext" in window), false);
      assert.equal(await page.$eval("#sound-on", (el) => el.disabled), true);
      assert.equal(await page.$eval("#sound-on", (el) => el.checked), false);
      assert.deepEqual(
        await page.$$eval("[data-sound], [data-sound-kind]", (els) => els.map((el) => el.disabled)),
        Array(10).fill(true),
      );
      assert.equal(await text("#sound-help"), "This browser cannot play sounds.");
    });

    assert.deepEqual(errors, []);
  });
});
