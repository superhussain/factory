// Capture entry: renders the real Factory screen with the owned stubs.
// tools/capture-plugin-cards.mjs maps factory-capture:screen to the screen file.
import { createRoot } from "react-dom/client";
import { dashboardScreen } from "factory-capture:screen";

// Dark is a synthetic guess. Light copies Paseo 0.16.2's light theme tokens, which the plugin SDK does not expose.
const THEMES = {
  dark: {
    colors: {
      surface0: "#111113",
      surface1: "#1a1a1d",
      surface2: "#26262a",
      foreground: "#ededef",
      foregroundMuted: "#a0a0a8",
      border: "#2e2e33",
      accent: "#3b82f6",
      accentForeground: "#ffffff",
    },
  },
  light: {
    colors: {
      surface0: "#ffffff",
      surface1: "#fafafa",
      surface2: "#f4f4f5",
      foreground: "#1a1a1e",
      foregroundMuted: "#71717a",
      border: "#e4e4e7",
      accent: "#20744A",
      accentForeground: "#ffffff",
    },
  },
};
const THEME = THEMES[globalThis.FACTORY_CAPTURE_SCHEME] ?? THEMES.dark;

const Screen = dashboardScreen({ openSettings() {} }, { fire() {}, listen: () => () => {} });

createRoot(document.getElementById("root")).render(
  <Screen theme={THEME} layout={{ compact: window.innerWidth < 600 }} params={{}} />,
);
