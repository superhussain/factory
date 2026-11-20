// Capture stub for react-native: plain DOM elements with the flexbox defaults
// React Native applies, enough for tools/capture-plugin-cards.mjs to lay out the
// Factory screen cards. An approximation of Paseo's renderer, not a port.
export const Platform = { OS: "web" };

export function flatten(style) {
  const out = {};
  const merge = (item) => {
    if (!item) return;
    if (Array.isArray(item)) item.forEach(merge);
    else Object.assign(out, item);
  };
  merge(style);
  if ("paddingVertical" in out) {
    out.paddingTop = out.paddingBottom = out.paddingVertical;
    delete out.paddingVertical;
  }
  if ("paddingHorizontal" in out) {
    out.paddingLeft = out.paddingRight = out.paddingHorizontal;
    delete out.paddingHorizontal;
  }
  if (typeof out.lineHeight === "number") out.lineHeight = `${out.lineHeight}px`;
  return out;
}

const BASE = {
  display: "flex",
  flexDirection: "column",
  position: "relative",
  boxSizing: "border-box",
  alignItems: "stretch",
  flexShrink: 0,
  minWidth: 0,
  minHeight: 0,
  margin: 0,
  padding: 0,
  borderWidth: 0,
  borderStyle: "solid",
};

export function View({ nativeID, style, children }) {
  return (
    <div id={nativeID} style={{ ...BASE, ...flatten(style) }}>
      {children}
    </div>
  );
}

export function Text({ style, children }) {
  return (
    <span
      style={{
        whiteSpace: "pre-wrap",
        overflowWrap: "break-word",
        fontFamily: 'system-ui, -apple-system, "Segoe UI", sans-serif',
        fontSize: 14,
        margin: 0,
        ...flatten(style),
      }}
    >
      {children}
    </span>
  );
}

export function Pressable({ style, onPress, accessibilityRole, children }) {
  return (
    <div
      role={accessibilityRole ?? "button"}
      tabIndex={0}
      onClick={onPress}
      style={{ ...BASE, cursor: "pointer", ...flatten(style) }}
    >
      {children}
    </div>
  );
}

export function ScrollView({ style, contentContainerStyle, children }) {
  return (
    <div style={{ ...BASE, flexGrow: 1, flexShrink: 1, overflow: "auto", ...flatten(style) }}>
      <div style={{ ...BASE, ...flatten(contentContainerStyle) }}>{children}</div>
    </div>
  );
}
