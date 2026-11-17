// Records a 1 s, 160x90, solid green WebM in the pinned browser, so tests attach a real clip.
export async function recordClip(browser, origin) {
  const context = await browser.createBrowserContext();
  try {
    const page = await context.newPage();
    await page.goto(`${origin}/`, { waitUntil: "load" });
    let timer;
    const timeout = new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error("recordClip: no clip within 30 s")), 30000);
    });
    const recording = page.evaluate(async () => {
      const canvas = document.createElement("canvas");
      canvas.width = 160;
      canvas.height = 90;
      const ctx = canvas.getContext("2d");
      const paint = () => {
        ctx.fillStyle = "#1d6b30";
        ctx.fillRect(0, 0, 160, 90);
      };
      paint();
      const timer = setInterval(paint, 100);
      const recorder = new MediaRecorder(canvas.captureStream(10), {
        mimeType: "video/webm;codecs=vp8",
      });
      const chunks = [];
      recorder.ondataavailable = (e) => chunks.push(e.data);
      const stopped = new Promise((resolve) => (recorder.onstop = resolve));
      recorder.start();
      await new Promise((resolve) => setTimeout(resolve, 1000));
      recorder.stop();
      await stopped;
      clearInterval(timer);
      const bytes = new Uint8Array(await new Blob(chunks).arrayBuffer());
      let text = "";
      for (const byte of bytes) text += String.fromCharCode(byte);
      return btoa(text);
    });
    const base64 = await Promise.race([recording, timeout]).finally(() => clearTimeout(timer));
    return Buffer.from(base64, "base64");
  } finally {
    await context.close();
  }
}
