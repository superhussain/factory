// Board ready: the dashboard's first board read has rendered (poll sets the top-level loaded flag after renderBoard).
async function waitForBoard(page) {
  await page.waitForFunction(() => loaded === true, { polling: 50 });
}
// Every poll re-renders the board and replaces each card.
// Click in-page so the card cannot detach between find and click.
const clickNow = (page, selector) => page.$eval(selector, (el) => el.click());
module.exports = { waitForBoard, clickNow };
