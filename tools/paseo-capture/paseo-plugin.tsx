// Capture stub for @getpaseo/plugin, /client and /client/ui: synthetic ready
// settings and placeholder UI parts for tools/capture-plugin-cards.mjs.
export const defineSettings = (definition) => definition;

export const useSettings = () => ({
  status: "ready",
  values: { origin: "https://factory.example.test", factoryLinkButtons: false },
});

export const openExternalUrl = async () => {};

export const useToast = () => ({ error() {} });

// Lucide icon paths (ISC license) for the names the Factory screen uses, copied from the Paseo 0.16.2 app.
const LUCIDE = {
  Factory: [
    "M12 16h.01",
    "M16 16h.01",
    "M3 19a2 2 0 0 0 2 2h14a2 2 0 0 0 2-2V8.5a.5.5 0 0 0-.769-.422l-4.462 2.844A.5.5 0 0 1 15 10.5v-2a.5.5 0 0 0-.769-.422L9.77 10.922A.5.5 0 0 1 9 10.5V5a2 2 0 0 0-2-2H5a2 2 0 0 0-2 2z",
    "M8 16h.01",
  ],
  LogIn: ["m10 17 5-5-5-5", "M15 12H3", "M15 3h4a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2h-4"],
  RotateCw: ["M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8", "M21 3v5h-5"],
  ExternalLink: [
    "M15 3h6v6",
    "M10 14 21 3",
    "M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6",
  ],
};

export function Icon({ name, size = 24, color = "currentColor" }) {
  const paths = LUCIDE[name];
  if (!paths) return null;
  return (
    <svg
      data-icon={name}
      aria-hidden="true"
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke={color}
      strokeWidth={2}
      strokeLinecap="round"
      strokeLinejoin="round"
      style={{ flexShrink: 0 }}
    >
      {paths.map((d) => (
        <path key={d} d={d} />
      ))}
    </svg>
  );
}

export const Loading = () => <span>Loading</span>;

export function ExternalLink({ href, children }) {
  return <a href={href}>{children}</a>;
}

export { ScrollView } from "./react-native.tsx";
