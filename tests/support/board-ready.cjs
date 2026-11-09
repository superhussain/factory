// Board ready: the dashboard's first board read has rendered (poll sets the top-level loaded flag after renderBoard).
async function waitForBoard(page) {
  await page.waitForFunction(() => loaded === true, { polling: 50 });
}
// Every poll re-renders the board and replaces each card.
// Find and click in one evaluate so the card cannot detach between them. page.$eval
// finds and clicks in two protocol calls, and a re-render in between eats the click.
const clickNow = (page, selector) =>
  page.evaluate((sel) => document.querySelector(sel).click(), selector);
// Opens the drawer's job actions menu so its items can be clicked.
async function openJobActions(page) {
  if ((await page.$eval("#d-actions", (el) => el.getAttribute("aria-expanded"))) !== "true") {
    await page.click("#d-actions");
  }
  await page.waitForSelector("#d-actions-menu:not([hidden])");
}
module.exports = { waitForBoard, clickNow, openJobActions };
