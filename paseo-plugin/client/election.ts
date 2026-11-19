import { settingsRpc } from "@getpaseo/plugin";
import type { PluginClientContext } from "@getpaseo/plugin/client";
import { joinBallot, sharedBallot } from "../shared/ballot";
import { runElection } from "../shared/election";
import { parseDashboardOrigin } from "../shared/embed";
import { factorySettings } from "../shared/settings";
import { onWindowFocus } from "./web";

export function startElection(
  client: PluginClientContext,
  register: (origin: string | null) => () => void,
) {
  const member = joinBallot(sharedBallot(globalThis), register);
  const read = settingsRpc(factorySettings.id).read;
  const readOrigin = async () => {
    const result = await client.rpc(read, {});
    const parsed =
      result.status === "ready" ? factorySettings.schema.safeParse(result.values) : null;
    return parsed?.success ? parseDashboardOrigin(parsed.data.origin) : null;
  };
  return { elected: member.elected, ...runElection(member, readOrigin, onWindowFocus) };
}
