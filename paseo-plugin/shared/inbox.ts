import { defineSubscription } from "@getpaseo/plugin";
import { z } from "zod";
import { JOB_ID } from "./embed";

export const factoryInbox = defineSubscription({
  name: "factory.inbox",
  input: z.object({}),
  event: z.object({
    count: z.number().int().min(0),
    jobs: z
      .array(
        z.object({
          id: z.string().regex(JOB_ID),
          project: z.string().max(128),
          title: z.string().max(200),
        }),
      )
      .max(500),
  }),
});

export type InboxEvent = z.output<typeof factoryInbox.event>;
