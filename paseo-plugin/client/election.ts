import { settingsRpc } from "@getpaseo/plugin";
import type { PluginClientContext } from "@getpaseo/plugin/client";
import { joinBallot, sharedBallot } from "../shared/ballot";
import { parseDashboardOrigin } from "../shared/embed";
import { factorySettings } from "../shared/settings";
import { onWindowFocus } from "./web";

export const ELECTION_POLL_MS = 30_000;
export const ELECTION_TIMEOUT_MS = 5_000;

export function startElection(
  client: PluginClientContext,
  register: (origin: string | null) => () => void,
  onSettings: (origin: string | null, factoryLinkButtons: boolean) => void,
) {
  const member = joinBallot(sharedBallot(globalThis), register);
  const read = settingsRpc(factorySettings.id).read;
  let started = 0;
  let settled = 0;
  const check = async () => {
    const seq = ++started;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let report: () => void;
    try {
      const result = await Promise.race([
        client.rpc(read, {}),
        new Promise<never>((_, reject) => {
          timer = setTimeout(() => reject(new Error("timeout")), ELECTION_TIMEOUT_MS);
        }),
      ]);
      const parsed =
        result.status === "ready" ? factorySettings.schema.safeParse(result.values) : null;
      const origin = parsed?.success ? parseDashboardOrigin(parsed.data.origin) : null;
      report = () => {
        member.online(origin);
        onSettings(origin, parsed?.success ? parsed.data.factoryLinkButtons : false);
      };
    } catch {
      report = () => member.offline();
    } finally {
      clearTimeout(timer);
    }
    // A slower, older check must not override a newer result.
    if (seq < settled) return;
    settled = seq;
    report();
  };
  void check();
  const poll = setInterval(() => void check(), ELECTION_POLL_MS);
  const stopFocus = onWindowFocus(() => void check());
  return {
    elected: member.elected,
    recheck: () => void check(),
    stop() {
      clearInterval(poll);
      stopFocus();
      member.leave();
    },
  };
}
