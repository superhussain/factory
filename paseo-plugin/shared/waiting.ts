import { defineSubscription } from "@getpaseo/plugin";
import { z } from "zod";
import { JOB_ID } from "./embed";

export const WAITING_POLL_MS = 60_000;

export const waitingJob = z.object({
  id: z.string().regex(JOB_ID),
  project: z.string().max(128),
  title: z.string().max(200),
});

export const waitingOutput = z.object({ jobs: z.array(waitingJob).max(500) });

export const factoryWaiting = defineSubscription({
  name: "factory.waiting",
  input: z.object({}),
  event: z.union([
    z.object({ type: z.literal("jobs"), jobs: z.array(waitingJob).max(500) }),
    z.object({ type: z.literal("error"), message: z.string().max(300) }),
  ]),
});
