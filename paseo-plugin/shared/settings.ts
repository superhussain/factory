import { defineSettings } from "@getpaseo/plugin";
import { z } from "zod";

export const factorySettings = defineSettings({
  id: "dashboard",
  scope: "host",
  version: 1,
  schema: z.object({ origin: z.string().default("") }),
});
