import { defineSubscription } from "@getpaseo/plugin";
import { z } from "zod";

export const factoryInbox = defineSubscription({
  name: "factory.inbox",
  input: z.object({}),
  event: z.object({ count: z.number().int().min(0) }),
});
