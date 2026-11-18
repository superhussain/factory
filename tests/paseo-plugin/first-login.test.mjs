// First-login helpers the Paseo plugin uses to skip onboarding: a per-origin seen flag in
// web storage, nothing on native, and storage errors that read as a first login.
import assert from "node:assert/strict";
import test from "node:test";
import {
  firstLoginSteps,
  hasLoadedBefore,
  isBrowserOrigin,
  markLoaded,
  openLabel,
  paseoOrigin,
} from "../../paseo-plugin/shared/first-login.ts";

const ORIGIN = "https://factory.example.test";

function fakeWeb() {
  const store = new Map();
  const calls = { storage: 0, origin: 0 };
  const web = {
    storage() {
      calls.storage++;
      return {
        getItem: (key) => store.get(key) ?? null,
        setItem: (key, value) => void store.set(key, value),
      };
    },
    origin() {
      calls.origin++;
      return ORIGIN;
    },
  };
  return { web, store, calls };
}

test("the seen key is per origin", () => {
  const { web, store } = fakeWeb();
  assert.equal(hasLoadedBefore(web, ORIGIN), false);
  markLoaded(web, ORIGIN);
  assert.deepEqual([...store], [["factory-plugin:loaded:" + ORIGIN, "1"]]);
  assert.equal(hasLoadedBefore(web, ORIGIN), true);
  assert.equal(hasLoadedBefore(web, "https://other.example.test"), false);
});

test("native skips storage and has no origin", () => {
  const { store, calls } = fakeWeb();
  assert.equal(hasLoadedBefore(null, ORIGIN), false);
  markLoaded(null, ORIGIN);
  assert.equal(paseoOrigin(null), null);
  assert.equal(store.size, 0);
  assert.deepEqual(calls, { storage: 0, origin: 0 });
});

test("storage errors read as not loaded and never throw", () => {
  const fail = () => {
    throw new Error("storage blocked");
  };
  const getItemThrows = { storage: () => ({ getItem: fail, setItem() {} }), origin: () => ORIGIN };
  assert.equal(hasLoadedBefore(getItemThrows, ORIGIN), false);

  const setItemThrows = {
    storage: () => ({ getItem: () => null, setItem: fail }),
    origin: () => ORIGIN,
  };
  assert.doesNotThrow(() => markLoaded(setItemThrows, ORIGIN));

  const storageThrows = { storage: fail, origin: () => ORIGIN };
  assert.equal(hasLoadedBefore(storageThrows, ORIGIN), false);
  assert.doesNotThrow(() => markLoaded(storageThrows, ORIGIN));
});

test("the web origin comes from the page", () => {
  const { web, calls } = fakeWeb();
  assert.equal(paseoOrigin(web), ORIGIN);
  assert.equal(calls.origin, 1);
});

test("in a browser, step 1 sends Safari users to a browser tab", () => {
  const steps = firstLoginSteps(true);
  assert.equal(steps.length, 4);
  assert.match(steps[0], /Open in a browser tab/);
});

test("desktop keeps the three Sign in steps", () => {
  assert.deepEqual(firstLoginSteps(false), [
    "Select Sign in. A small Paseo window opens on the Cloudflare Access page.",
    "Enter your email, then the code Cloudflare sends you.",
    "That window closes once your board shows, and this page reloads. If it stays open, close it.",
  ]);
});

test("the open button names a tab in a browser and the browser on desktop", () => {
  assert.equal(openLabel(true), "Open in a browser tab");
  assert.equal(openLabel(false), "Open in your browser");
});

test("only http and https Paseo origins count as a browser", () => {
  assert.equal(isBrowserOrigin("https://app.paseo.sh"), true);
  assert.equal(isBrowserOrigin("paseo://app"), false);
  assert.equal(isBrowserOrigin(null), false);
});
