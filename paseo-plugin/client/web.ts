import { createElement, useEffect, useRef } from "react";
import { Platform } from "react-native";
import { readEmbedMessage, type EmbedMessage } from "../shared/embed";

// This plugin typechecks without the DOM library. Declare only what this module uses.
type MessageSource = { postMessage(message: unknown, targetOrigin: string): void };
type FrameMessage = { source: unknown; origin: string; data: unknown };
declare const window: {
  addEventListener(type: "message", listener: (event: FrameMessage) => void): void;
  removeEventListener(type: "message", listener: (event: FrameMessage) => void): void;
};

const SANDBOX =
  "allow-scripts allow-same-origin allow-forms allow-popups allow-popups-to-escape-sandbox allow-downloads allow-modals";

export function DashboardFrame({
  src,
  origin,
  onMessage,
}: {
  src: string;
  origin: string;
  onMessage(message: EmbedMessage): void;
}) {
  const frame = useRef<{ contentWindow: MessageSource | null } | null>(null);
  const handler = useRef(onMessage);
  handler.current = onMessage;
  useEffect(() => {
    if (Platform.OS !== "web") return;
    const listener = (event: FrameMessage) => {
      if (!frame.current || event.source !== frame.current.contentWindow || event.origin !== origin)
        return;
      const message = readEmbedMessage(event.data);
      if (message) handler.current(message);
    };
    window.addEventListener("message", listener);
    return () => window.removeEventListener("message", listener);
  }, [origin]);
  if (Platform.OS !== "web") return null;
  return createElement("iframe", {
    src,
    title: "Factory dashboard",
    sandbox: SANDBOX,
    style: { border: 0, width: "100%", height: "100%" },
    ref: frame,
    onLoad: () =>
      frame.current?.contentWindow?.postMessage({ type: "factory-embed-hello", v: 1 }, origin),
  });
}
