// The plugin card capture bundles the real Factory screen and only owned stubs for
// Paseo, React Native and the frame module. No browser needed.
import assert from "node:assert/strict";
import test from "node:test";
import { fileURLToPath } from "node:url";
import * as esbuild from "esbuild";
import { bundleCards } from "../../tools/capture-plugin-cards.mjs";

test("the card capture bundles the real screen with owned stubs", async () => {
  const { code, inputs } = await bundleCards();
  for (const file of [
    "paseo-plugin/client/dashboard-screen.tsx",
    "paseo-plugin/shared/first-login.ts",
    "tools/paseo-capture/entry.tsx",
    "tools/paseo-capture/react-native.tsx",
    "tools/paseo-capture/paseo-plugin.tsx",
    "tools/paseo-capture/web.tsx",
  ])
    assert.ok(inputs.includes(file), `${file} is in the bundle`);
  for (const input of inputs) {
    assert.ok(!input.includes("@getpaseo"), `${input} is not Paseo package code`);
    assert.doesNotMatch(input, /react-native[@/]/, `${input} is not react-native package code`);
    assert.notEqual(input, "paseo-plugin/client/web.ts", "the real frame module is stubbed");
  }
  for (const title of [
    "Your sign-in has expired",
    "Factory did not load",
    "Sign in to your factory",
  ])
    assert.ok(code.includes(title), `the bundle holds "${title}"`);
});

test("the View stub renders nativeID as the element id", async () => {
  const repo = fileURLToPath(new URL("../..", import.meta.url));
  const { outputFiles } = await esbuild.build({
    entryPoints: [repo + "tools/paseo-capture/react-native.tsx"],
    bundle: true,
    write: false,
    format: "iife",
    globalName: "out",
    platform: "browser",
    jsx: "automatic",
    nodePaths: [repo + "paseo-plugin/node_modules"],
  });
  const { View } = new Function(outputFiles[0].text + "\nreturn out;")();
  const withId = View({ nativeID: "factory-plugin-card" });
  const withoutId = View({});
  assert.equal(withId.type, "div");
  assert.equal(withId.props.id, "factory-plugin-card");
  assert.equal(withoutId.type, "div");
  assert.equal(withoutId.props.id, undefined);
});
