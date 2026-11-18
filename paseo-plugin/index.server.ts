import type { PluginServerContext } from "@getpaseo/plugin/server";
import { factorySettings } from "./shared/settings";

export default function contribute(server: PluginServerContext) {
  server.registerSettings(factorySettings);
  return () => {};
}
