import { build } from "esbuild";
import { createHash } from "node:crypto";

async function bundle(entry) {
  const result = await build({
    entryPoints: [new URL(entry, import.meta.url).pathname],
    bundle: true,
    write: false,
    format: "esm",
    platform: "neutral",
    target: "es2022",
    loader: { ".html": "text", ".txt": "text", ".md": "text" },
  });
  return import(
    "data:text/javascript;base64," + Buffer.from(result.outputFiles[0].contents).toString("base64")
  );
}

export function loadProduction() {
  return bundle("../../worker/index.js");
}

export function loadAssets() {
  return bundle("../../worker/assets.js");
}

export function memoryBucket() {
  const objects = new Map();
  const calls = [];
  const tag = (bytes) => '"' + createHash("sha256").update(bytes).digest("hex") + '"';
  return {
    objects,
    calls,
    async head(key) {
      calls.push(["head", key]);
      const object = objects.get(key);
      return object ? { ...object, body: undefined } : null;
    },
    async get(key) {
      calls.push(["get", key]);
      const object = objects.get(key);
      return object
        ? { ...object, body: object.bytes, text: async () => object.bytes.toString() }
        : null;
    },
    async put(key, value, options = {}) {
      calls.push(["put", key, options]);
      const previous = objects.get(key);
      const condition = options.onlyIf;
      if (
        (condition?.has("If-Match") && previous?.httpEtag !== condition.get("If-Match")) ||
        (condition?.get("If-None-Match") === "*" && previous)
      )
        return null;
      const bytes = Buffer.from(value);
      const object = {
        key,
        bytes,
        httpEtag: tag(bytes),
        httpMetadata: options.httpMetadata,
        customMetadata: options.customMetadata,
        options,
        uploaded: new Date(),
      };
      objects.set(key, object);
      return object;
    },
    async delete(key) {
      calls.push(["delete", key]);
      objects.delete(key);
    },
    async list(options) {
      calls.push(["list", options]);
      return {
        objects: [...objects.values()]
          .filter((object) => object.key.startsWith(options.prefix))
          .sort((a, b) => a.key.localeCompare(b.key))
          .slice(0, 1000),
      };
    },
  };
}

export const serverEnv = {
  FACTORY_ORIGIN: "https://factory.example.test",
  FACTORY_TITLE: "Factory",
  ACCESS_ISSUER: "https://example-team.cloudflareaccess.com",
  ACCESS_AUDIENCE: "example-application-audience",
};
