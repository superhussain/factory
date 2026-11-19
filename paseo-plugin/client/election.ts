import { settingsRpc } from "@getpaseo/plugin";
import type { PluginClientContext } from "@getpaseo/plugin/client";
import { joinBallot, sharedBallot } from "../shared/ballot";
import { parseDashboardOrigin } from "../shared/embed";
import { factorySettings } from "../shared/settings";
import { onWindowFocus } from "./web";

export const ELECTION_POLL_MS = 30_000;
export const ELECTION_TIMEOUT_MS = 5_000;

export function startElection(client: PluginClientContext, register: () => () => void) {
  const member = joinBallot(sharedBallot(globalThis), register);
  const read = settingsRpc(factorySettings.id).read;
  const check = async () => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const result = await Promise.race([
        client.rpc(read, {}),
        new Promise<never>((_, reject) => {
          timer = setTimeout(() => reject(new Error("timeout")), ELECTION_TIMEOUT_MS);
        }),
      ]);
      const parsed =
        result.status === "ready" ? factorySettings.schema.safeParse(result.values) : null;
      member.online(parsed?.success ? parseDashboardOrigin(parsed.data.origin) : null);
    } catch {
      member.offline();
    } finally {
      clearTimeout(timer);
    }
  };
  void check();
  const poll = setInterval(() => void check(), ELECTION_POLL_MS);
  const stopFocus = onWindowFocus(() => void check());
  return {
    elected: member.elected,
    stop() {
      clearInterval(poll);
      stopFocus();
      member.leave();
    },
  };
}
