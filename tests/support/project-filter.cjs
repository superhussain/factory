// The header project filter: a button with a checkbox menu, one box per project key.
async function filterState(page) {
  return page.evaluate(() => ({
    label: document.getElementById("filter-label").textContent,
    rows: [...document.querySelectorAll("#filter-menu [data-project]")].map((box) => ({
      key: box.dataset.project,
      title: box.closest("label").textContent.trim(),
      checked: box.checked,
      mixed: box.indeterminate,
    })),
    stored: localStorage.getItem("factory-projects-hidden"),
  }));
}
// Shows only key ('' shows every key) by clicking boxes under root. Focus does not move.
async function showOnly(page, key, root = "#filter-menu") {
  await page.evaluate(
    (r, k) => {
      const box = (x) => document.querySelector(`${r} [data-project="${CSS.escape(x)}"]`);
      if (!box("").checked) box("").click();
      if (k === "") return;
      box("").click();
      box(k).click();
    },
    root,
    key,
  );
}
// Serves list as projects.json to this page only, so a case never changes the shared origin.
async function serveProjects(page, list) {
  await page.setBypassServiceWorker(true);
  await page.setRequestInterception(true);
  page.on("request", (request) =>
    new URL(request.url()).pathname.endsWith("/projects.json")
      ? request.respond({
          status: 200,
          contentType: "application/json",
          body: JSON.stringify(list),
        })
      : request.continue(),
  );
}
module.exports = { filterState, showOnly, serveProjects };
