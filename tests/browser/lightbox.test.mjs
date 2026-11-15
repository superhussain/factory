// One full-screen viewer opens from the New idea thumbnails, the drawer's pasted-image
// gallery and the images on a job page, and cycles through that place's images.
import assert from "node:assert/strict";
import test from "node:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { crc32, deflateSync } from "node:zlib";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";

// A solid-colour RGB PNG, so each image in the set is told apart by its bytes.
function png(w, h, [r, g, b]) {
  const chunk = (type, data) => {
    const len = Buffer.alloc(4);
    len.writeUInt32BE(data.length);
    const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(body));
    return Buffer.concat([len, body, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8;
  ihdr[9] = 2;
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array(w).fill([r, g, b]).flat())]);
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk("IHDR", ihdr),
    chunk("IDAT", deflateSync(Buffer.concat(Array(h).fill(row)))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

const SEED = "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n";
const PASTED = [png(320, 200, [33, 118, 174]), png(320, 200, [240, 162, 2])];
const NAMES = PASTED.map((b) => createHash("sha256").update(b).digest("hex") + ".png");
const PAGE_IMAGES = {
  "before.png": png(480, 300, [155, 45, 134]),
  "after.png": png(480, 300, [60, 157, 78]),
};
const MOBILE = { width: 390, height: 844, isMobile: true, hasTouch: true };
const DESKTOP = { width: 1280, height: 900 };
const wait = { polling: 50 };
const captureDir = process.env.FACTORY_CAPTURE_DIR;
if (captureDir) await mkdir(captureDir, { recursive: true });

test("lightbox", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  const files = await mkdtemp(join(tmpdir(), "lightbox-"));
  try {
    await withBrowserFixture(async ({ browser, origin }) => {
      await seedProject(origin, "sample", SEED);
      for (const [i, body] of PASTED.entries()) {
        const r = await fetch(`${origin}/api/idea-images/${NAMES[i]}`, {
          method: "PUT",
          headers: { "Content-Type": "image/png" },
          body,
        });
        assert.equal(r.status, 200, NAMES[i]);
      }
      await seedBoard(origin, [
        {
          id: "001-photos",
          project: "sample",
          title: "Synthetic 001-photos",
          stage: "spec",
          updated: "2026-01-01T00:00:00Z",
          idea: "Show pasted photos full screen.",
          idea_images: NAMES,
        },
      ]);
      for (const [name, body, type] of [
        ["spec.md", "# Spec\n\n![before](before.png)\n\n![after](after.png)\n", "text/markdown"],
        ...Object.entries(PAGE_IMAGES).map(([n, b]) => [n, b, "image/png"]),
      ]) {
        const r = await fetch(`${origin}/api/jobs/sample/001-photos/${name}`, {
          method: "PUT",
          headers: { "If-None-Match": "*", "Content-Type": type },
          body,
        });
        assert.equal(r.status, 200, name);
      }
      const uploads = [];
      for (const [i, body] of PASTED.entries()) {
        uploads.push(join(files, `pasted-${i + 1}.png`));
        await writeFile(uploads[i], body);
      }

      const page = await (await browser.createBrowserContext()).newPage();
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      const capture = (name) => captureDir && page.screenshot({ path: join(captureDir, name) });
      const viewer = () =>
        page.evaluate(() => {
          const lb = document.getElementById("lightbox");
          return {
            open: lb.open,
            count: document.getElementById("lb-count").textContent,
            src: document.getElementById("lb-img").getAttribute("src"),
          };
        });
      const loaded = () =>
        page.waitForFunction(() => {
          const img = document.getElementById("lb-img");
          return img.complete && img.naturalWidth > 0;
        }, wait);
      const look = async (view, theme) => {
        await page.setViewport(view);
        await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: theme }]);
      };
      const openJob = async (tab, ready) => {
        await page.goto("about:blank");
        await page.goto(`${origin}/#001-photos/${tab}`, { waitUntil: "load" });
        await page.waitForFunction(ready, wait);
      };
      const thumb = (n) => `#idea-image-list li:nth-child(${n}) .thumb-btn`;

      await t.test("L1 idea thumbnail 2 opens Image 2 of 2", async () => {
        await look(MOBILE, "dark");
        await page.goto(`${origin}/`, { waitUntil: "load" });
        await page.waitForFunction(() => !document.getElementById("new-idea").hidden, wait);
        await page.click("#new-idea");
        await page.type("#idea-text", "Synthetic photo idea");
        await (await page.$("#idea-file")).uploadFile(...uploads);
        await page.waitForFunction(
          () =>
            draft.images.length === 2 &&
            [...document.querySelectorAll("#idea-image-list img")].every((i) => i.complete),
          wait,
        );
        await capture("lightbox-idea-390-before.png");
        await page.click(thumb(2));
        await loaded();
        const v = await viewer();
        assert.equal(v.open, true);
        assert.equal(v.count, "Image 2 of 2");
        assert.match(v.src, /^blob:/);
        await capture("lightbox-idea-390.png");
      });

      await t.test("L2 Next, Previous and arrow keys wrap; one image hides both", async () => {
        await page.click("#lb-next");
        assert.equal((await viewer()).count, "Image 1 of 2");
        await page.click("#lb-prev");
        assert.equal((await viewer()).count, "Image 2 of 2");
        await page.keyboard.press("ArrowRight");
        assert.equal((await viewer()).count, "Image 1 of 2");
        await page.keyboard.press("ArrowLeft");
        assert.equal((await viewer()).count, "Image 2 of 2");
        await page.click("#lb-close");
        await page.evaluate(() =>
          openLightbox([{ src: draft.images[0].url, alt: "Preview of image 1" }], 0),
        );
        assert.deepEqual(
          await page.evaluate(() => [
            document.getElementById("lb-prev").checkVisibility(),
            document.getElementById("lb-next").checkVisibility(),
            document.getElementById("lb-count").textContent,
          ]),
          [false, false, "Preview of image 1"],
        );
        await page.click("#lb-close");
      });

      await t.test("L3 Escape closes only the viewer and refocuses thumbnail 2", async () => {
        await page.click(thumb(2));
        await page.keyboard.press("Escape");
        await page.waitForFunction(() => !document.getElementById("lightbox").open, wait);
        assert.deepEqual(
          await page.evaluate(() => [
            document.getElementById("idea-dlg").open,
            document.getElementById("idea-text").value,
            draft.images.length,
            document.activeElement.getAttribute("aria-label"),
          ]),
          [true, "Synthetic photo idea", 2, "View image 2 larger"],
        );
      });

      await t.test("L4 removing image 1 focuses the remaining Remove button", async () => {
        await page.click("#idea-image-list li:nth-child(1) button.btn");
        assert.deepEqual(
          await page.evaluate(() => [
            draft.images.length,
            document.activeElement.classList.contains("thumb-btn"),
            document.activeElement.getAttribute("aria-label").startsWith("Remove: Image 1:"),
          ]),
          [1, false, true],
        );
        await page.click("#idea-cancel");
      });

      await t.test("L5 gallery image 1 opens Image 1 of 2; Escape keeps the drawer", async () => {
        await look(DESKTOP, "light");
        await openJob(
          "idea",
          () =>
            [...document.querySelectorAll("#d-gallery .gallery-open img")].filter(
              (i) => i.complete && i.naturalWidth,
            ).length === 2,
        );
        await capture("lightbox-gallery-1280-before.png");
        await page.click("#d-gallery figure:nth-child(1) .gallery-open");
        await loaded();
        const v = await viewer();
        assert.equal(v.count, "Image 1 of 2");
        assert.equal(v.src, `./api/idea-images/${NAMES[0]}`);
        await capture("lightbox-gallery-1280.png");
        await page.keyboard.press("Escape");
        await page.waitForFunction(() => !document.getElementById("lightbox").open, wait);
        assert.equal(await page.evaluate(() => openId), "001-photos");
      });

      await t.test("L6 a page image opens Image 1 of 2; a Meta-click is left alone", async () => {
        await look(MOBILE, "light");
        await openJob(
          "spec",
          () =>
            [...document.querySelectorAll("#d-doc a.img-link img")].filter(
              (i) => i.complete && i.naturalWidth,
            ).length === 2,
        );
        await capture("lightbox-page-390-before.png");
        await page.click("#d-doc a.img-link");
        await loaded();
        const v = await viewer();
        assert.equal(v.count, "Image 1 of 2");
        assert.match(v.src, /\/before\.png$/);
        await capture("lightbox-page-390.png");
        await page.click("#lb-next");
        assert.match((await viewer()).src, /\/after\.png$/);
        await page.click("#lb-close");
        assert.deepEqual(
          await page.evaluate(() => {
            let prevented;
            document.addEventListener(
              "click",
              (e) => {
                prevented = e.defaultPrevented;
                e.preventDefault();
              },
              { once: true },
            );
            document
              .querySelector("#d-doc a.img-link")
              .dispatchEvent(
                new MouseEvent("click", { bubbles: true, cancelable: true, metaKey: true }),
              );
            return [prevented, document.getElementById("lightbox").open];
          }),
          [false, false],
        );
      });

      await t.test("L7 a 120 px left swipe advances the image", async () => {
        await page.click("#d-doc a.img-link");
        await loaded();
        const cdp = await page.createCDPSession();
        const touch = (type, points) =>
          cdp.send("Input.dispatchTouchEvent", {
            type,
            touchPoints: points.map(([x, y]) => ({ x, y })),
          });
        await touch("touchStart", [[255, 422]]);
        for (let x = 235; x >= 135; x -= 20) await touch("touchMove", [[x, 422]]);
        await touch("touchEnd", []);
        assert.equal((await viewer()).count, "Image 2 of 2");
        await page.click("#lb-close");
      });

      await t.test("L8 the viewer fills the viewport", async () => {
        for (const view of [MOBILE, DESKTOP]) {
          // A change of isMobile reloads the page, so wait for the job page again.
          await page.setViewport(view);
          await page.waitForSelector("#d-doc a.img-link");
          await page.click("#d-doc a.img-link");
          const rect = await page.$eval("#lightbox", (el) => {
            const r = el.getBoundingClientRect();
            return [r.x, r.y, r.width, r.height];
          });
          assert.deepEqual(rect, [0, 0, view.width, view.height]);
          await page.click("#lb-close");
        }
      });

      assert.deepEqual(errors, []);
    });
  } finally {
    await rm(files, { recursive: true, force: true });
  }
});
