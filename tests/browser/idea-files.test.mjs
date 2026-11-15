// The New idea dialog attaches PDF, CSV, text, MP4 and WebM files, and the drawer lists them
// as download links. MP4 and WebM also play in the drawer.
import assert from "node:assert/strict";
import test from "node:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { prepareAssets } from "../../tools/assets.mjs";
import { withBrowserFixture, seedProject, seedBoard } from "../support/browser-server.mjs";
import { recordClip } from "../support/webm-clip.mjs";

const SEED = "---\nrepo: example/sample\nbase: main\ntitle: Synthetic sample\n---\n";
const FILES = {
  "brief.pdf": "%PDF-1.4\n%%EOF\n",
  "notes.csv": "region,total\nnorth,12\n",
  "evil.html": "<script>alert(1)</script>\n",
  "fake.pdf": "hello",
};
const name = (file, body = FILES[file]) =>
  createHash("sha256").update(body).digest("hex") + file.slice(file.lastIndexOf("."));
const BROKEN_MP4 = Buffer.concat([
  Buffer.from([0, 0, 0, 0x18]),
  Buffer.from("ftypisom"),
  Buffer.alloc(12),
]);
const MOBILE = { width: 390, height: 844 };
const DESKTOP = { width: 1280, height: 900 };
const wait = { polling: 50 };
const captureDir = process.env.FACTORY_CAPTURE_DIR;
if (captureDir) await mkdir(captureDir, { recursive: true });

test("idea files", { timeout: 180000 }, async (t) => {
  await prepareAssets();
  const dir = await mkdtemp(join(tmpdir(), "idea-files-"));
  try {
    const path = (file) => join(dir, file);
    for (const [file, body] of Object.entries(FILES)) await writeFile(path(file), body);
    await withBrowserFixture(async ({ browser, origin }) => {
      await seedProject(origin, "sample", SEED);
      await seedBoard(origin, []);
      const page = await (await browser.createBrowserContext()).newPage();
      const errors = [];
      page.on("pageerror", (error) => errors.push(String(error)));
      const capture = (file) => captureDir && page.screenshot({ path: join(captureDir, file) });
      const look = async (view) => {
        await page.setViewport(view);
        await page.emulateMediaFeatures([{ name: "prefers-color-scheme", value: "light" }]);
      };
      const rows = () =>
        page.$$eval("#idea-image-list li .grow", (spans) => spans.map((s) => s.textContent));
      const add = async (...files) => {
        await (await page.$("#idea-file")).uploadFile(...files.map(path));
        await page.waitForFunction(() => !draft.filePending, wait);
      };
      const remove = (text) => page.click(`#idea-image-list button[aria-label="Remove: ${text}"]`);
      let id;

      await t.test("F1 the dialog lists brief.pdf and notes.csv", async () => {
        await look(MOBILE);
        await page.goto(`${origin}/`, { waitUntil: "load" });
        await page.waitForFunction(() => !document.getElementById("new-idea").hidden, wait);
        await page.click("#new-idea");
        await page.type("#idea-text", "Import the attached sales sheet.");
        await add("brief.pdf", "notes.csv");
        assert.deepEqual(await rows(), [
          "File 1: brief.pdf, PDF, 1 KiB",
          "File 2: notes.csv, CSV, 1 KiB",
        ]);
        assert.equal(await page.$eval("#idea-upload", (b) => b.textContent), "Add files");
        assert.equal(
          await page.$eval("#idea-image-list", (l) => l.getAttribute("aria-label")),
          "Attachments",
        );
        await capture("idea-files-dialog-390.png");
      });

      await t.test("F2 evil.html is refused, and fake.pdf gets the Worker's refusal", async () => {
        await add("evil.html");
        const refused = "Not added: evil.html is not an image, PDF, CSV, text, MP4 or WebM file.";
        assert.equal((await rows()).at(-1), refused);
        await remove(refused);
        await add("fake.pdf");
        assert.equal((await rows()).at(-1), "File 3: fake.pdf, PDF, 1 KiB");
        await page.click("#idea-add");
        await page.waitForSelector("#idea-err:not([hidden])");
        assert.match(
          await page.$eval("#idea-err", (e) => e.textContent),
          /^uploading a file failed: 415 the body does not match the PDF format\./,
        );
        await remove("File 3: fake.pdf, PDF, 1 KiB");
        assert.deepEqual(await rows(), [
          "File 1: brief.pdf, PDF, 1 KiB",
          "File 2: notes.csv, CSV, 1 KiB",
        ]);
      });

      await t.test("F3 Add saves idea_files in order", async () => {
        await page.click("#idea-add");
        await page.waitForFunction(() => !document.getElementById("idea-dlg").open, wait);
        const board = await (await fetch(`${origin}/api/board`)).json();
        const job = board.jobs.find((j) => j.idea === "Import the attached sales sheet.");
        assert.deepEqual(job.idea_files, [name("brief.pdf"), name("notes.csv")]);
        assert.equal("idea_images" in job, false);
        id = job.id;
      });

      await t.test("F4 the drawer shows two download links", async () => {
        await look(DESKTOP);
        await page.goto("about:blank");
        await page.goto(`${origin}/#${id}/idea`, { waitUntil: "load" });
        await page.waitForFunction(
          () => document.querySelectorAll("#d-files a").length === 2,
          wait,
        );
        assert.deepEqual(
          await page.$$eval("#d-files a", (links) =>
            links.map((a) => [a.textContent, a.getAttribute("href"), a.getAttribute("download")]),
          ),
          [
            ["Download file 1 (PDF)", `./api/idea-files/${name("brief.pdf")}`, "idea-file-1.pdf"],
            ["Download file 2 (CSV)", `./api/idea-files/${name("notes.csv")}`, "idea-file-2.csv"],
          ],
        );
        const height = await page.$eval("#d-files a", (a) => a.getBoundingClientRect().height);
        assert.ok(height >= 44, String(height));
        await capture("idea-files-drawer-1280.png");
        await look(MOBILE);
        await page.goto("about:blank");
        await page.goto(`${origin}/#${id}/idea`, { waitUntil: "load" });
        await page.waitForFunction(
          () => document.querySelectorAll("#d-files a").length === 2,
          wait,
        );
        await page.evaluate(
          () =>
            new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve))),
        );
        assert.notEqual(
          await page.evaluate(() =>
            getComputedStyle(document.getElementById("detail")).getPropertyValue("--head-collapse"),
          ),
          "0px",
        );
      });

      await t.test("F5 the drawer plays clip.webm and falls back on broken.mp4", async () => {
        const clip = await recordClip(browser, origin);
        await writeFile(path("clip.webm"), clip);
        await writeFile(path("broken.mp4"), BROKEN_MP4);
        const clipName = name("clip.webm", clip);
        await look(MOBILE);
        await page.goto("about:blank");
        await page.goto(`${origin}/`, { waitUntil: "load" });
        await page.waitForFunction(() => !document.getElementById("new-idea").hidden, wait);
        await page.click("#new-idea");
        await page.type("#idea-text", "Play the attached screen clip.");
        await add("clip.webm", "broken.mp4");
        await page.click("#idea-add");
        await page.waitForFunction(() => !document.getElementById("idea-dlg").open, wait);
        const board = await (await fetch(`${origin}/api/board`)).json();
        const job = board.jobs.find((j) => j.idea === "Play the attached screen clip.");
        assert.deepEqual(job.idea_files, [clipName, name("broken.mp4", BROKEN_MP4)]);

        const responses = [];
        page.on("response", (response) => {
          if (response.url().endsWith(`/api/idea-files/${clipName}`))
            responses.push([response.status(), response.headers()["content-range"]]);
        });
        await page.goto("about:blank");
        await page.goto(`${origin}/#${job.id}/idea`, { waitUntil: "load" });
        await page.waitForFunction(
          () =>
            document.querySelectorAll("#d-files a").length === 2 &&
            document.querySelector("#d-files video")?.readyState >= 1 &&
            document.querySelector("#d-files p.bad"),
          wait,
        );
        assert.deepEqual(
          await page.$$eval("#d-files li", (items) =>
            items.map((li) => [
              li.firstElementChild.tagName,
              li.firstElementChild.getAttribute("aria-label") ?? li.firstElementChild.textContent,
              li.querySelector("a").textContent,
            ]),
          ),
          [
            ["VIDEO", "Video 1 (WebM)", "Download file 1 (WebM)"],
            ["P", "Video 2 cannot play here. Use its download link.", "Download file 2 (MP4)"],
          ],
        );
        assert.equal(await page.$$eval("#d-files video", (videos) => videos.length), 1);
        assert.equal(await page.$eval("#d-files video", (v) => v.videoWidth), 160);
        assert.ok(
          responses.some(([status, range]) => status === 206 && range?.startsWith("bytes 0-")),
          JSON.stringify(responses),
        );
        await capture("idea-video-drawer-390.png");

        const before = await page.$("#d-files video");
        await page.evaluate(() => renderDetail());
        assert.ok(
          await page.evaluate(
            (video) => video.isConnected && video === document.querySelector("#d-files video"),
            before,
          ),
        );
      });

      assert.deepEqual(errors, []);
    });
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
