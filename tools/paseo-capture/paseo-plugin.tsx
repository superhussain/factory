// Capture stub for @getpaseo/plugin, /client and /client/ui: synthetic ready
// settings and placeholder UI parts for tools/capture-plugin-cards.mjs.
export const defineSettings = (definition) => definition;

export const useSettings = () => ({
  status: "ready",
  values: { origin: "https://factory.example.test", factoryLinkButtons: false },
});

export const openExternalUrl = async () => {};

export const useToast = () => ({ error() {} });

export function Icon({ name, size, color }) {
  return (
    <span
      data-icon={name}
      aria-hidden="true"
      style={{
        display: "inline-block",
        width: size,
        height: size,
        border: `2px solid ${color}`,
        borderRadius: 4,
        boxSizing: "border-box",
        flexShrink: 0,
      }}
    />
  );
}

export const Loading = () => <span>Loading</span>;

export function ExternalLink({ href, children }) {
  return <a href={href}>{children}</a>;
}

export { ScrollView } from "./react-native.tsx";
