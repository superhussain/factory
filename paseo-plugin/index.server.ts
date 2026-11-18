import type { PluginServerContext } from "@getpaseo/plugin/server";
import { INBOX_POLL_MS } from "./shared/badge";
import { factoryInbox } from "./shared/inbox";
import { factorySettings } from "./shared/settings";
import { findRuntime, readInbox } from "./server/inbox";

export default function contribute(server: PluginServerContext) {
  server.registerSettings(factorySettings);
  server.handle(factoryInbox, (_input, { push, signal }) => {
    const poll = async () => {
      const runtime = findRuntime();
      const event = runtime ? await readInbox(runtime) : null;
      if (event && !signal.aborted) await push(event);
    };
    void poll();
    const timer = setInterval(() => void poll(), INBOX_POLL_MS);
    signal.addEventListener("abort", () => clearInterval(timer), { once: true });
  });
  return () => {};
}
