// The part of workerd's cloudflare:workers module the Worker uses, for bundles that run in Node.
export class DurableObject {
  constructor(ctx, env) {
    this.ctx = ctx;
    this.env = env;
  }
}
