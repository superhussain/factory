// Board ready: the dashboard's first board read has rendered (poll sets the top-level loaded flag after renderBoard).
async function waitForBoard(page) {
  await page.waitForFunction(() => loaded === true, { polling: 50 });
}
// Every poll re-renders the board and replaces each card.
// Find and click in one evaluate so the card cannot detach between them. page.$eval
// finds and clicks in two protocol calls, and a re-render in between eats the click.
const clickNow = (page, selector) =>
  page.evaluate((sel) => document.querySelector(sel).click(), selector);
module.exports = { waitForBoard, clickNow };
