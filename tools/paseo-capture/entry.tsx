// Capture entry: renders the real Factory screen with the owned stubs.
// tools/capture-plugin-cards.mjs maps factory-capture:screen to the screen file.
import { createRoot } from "react-dom/client";
import { dashboardScreen } from "factory-capture:screen";

// Synthetic dark palette, a guess and not Paseo's real theme.
const THEME = {
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
};

const Screen = dashboardScreen({ openSettings() {} }, { fire() {}, listen: () => () => {} });

createRoot(document.getElementById("root")).render(
  <Screen theme={THEME} layout={{ compact: window.innerWidth < 600 }} params={{}} />,
);
