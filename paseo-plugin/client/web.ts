import { openExternalUrl } from "@getpaseo/plugin/client";
import { createElement, useEffect, useRef } from "react";
import { Platform } from "react-native";
import {
  dashboardSrc,
  frameLoaded,
  readEmbedMessage,
  readSignedInMessage,
  type EmbedMessage,
} from "../shared/embed";
import * as firstLogin from "../shared/first-login";
import { setFrameInboxCount } from "./frame-inbox";

// This plugin typechecks without the DOM library. Declare only what this module uses.
type MessageSource = { postMessage(message: unknown, targetOrigin: string): void };
type FrameMessage = { source: unknown; origin: string; data: unknown };
type SignInWindow = { closed: boolean; close(): void; focus(): void };
declare const window: {
  open(url: string, target: string, features: string): SignInWindow | null;
  addEventListener(type: "message", listener: (event: FrameMessage) => void): void;
  removeEventListener(type: "message", listener: (event: FrameMessage) => void): void;
};
declare const localStorage: {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
};
declare const location: { origin: string };

const WEB: firstLogin.WebGlobals = { storage: () => localStorage, origin: () => location.origin };

function webGlobals() {
  return Platform.OS === "web" ? WEB : null;
}

export function hasLoadedBefore(origin: string): boolean {
  return firstLogin.hasLoadedBefore(webGlobals(), origin);
}

export function markLoaded(origin: string): void {
  firstLogin.markLoaded(webGlobals(), origin);
}

export function paseoOrigin(): string | null {
  return firstLogin.paseoOrigin(webGlobals());
}

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
    onLoad: () => frameLoaded(frame.current?.contentWindow, origin, setFrameInboxCount),
  });
}

export function useSignInPopup(origin: string, onClosed: () => void): () => void {
  const popup = useRef<SignInWindow | null>(null);
  const timer = useRef<ReturnType<typeof setInterval> | null>(null);
  const closed = useRef(onClosed);
  closed.current = onClosed;
  useEffect(() => {
    if (Platform.OS !== "web") return;
    const listener = (event: FrameMessage) => {
      if (
        popup.current &&
        event.source === popup.current &&
        event.origin === origin &&
        readSignedInMessage(event.data)
      )
        popup.current.close();
    };
    window.addEventListener("message", listener);
    return () => {
      window.removeEventListener("message", listener);
      if (timer.current) clearInterval(timer.current);
      popup.current = null;
    };
  }, [origin]);
  return () => {
    if (Platform.OS !== "web") return void openExternalUrl(origin);
    if (popup.current) return popup.current.focus();
    const opened = window.open(
      dashboardSrc(origin),
      "factory-sign-in",
      "popup,width=480,height=720",
    );
    if (!opened) return void openExternalUrl(origin);
    popup.current = opened;
    timer.current = setInterval(() => {
      if (!opened.closed) return;
      if (timer.current) clearInterval(timer.current);
      popup.current = null;
      closed.current();
    }, 500);
  };
}
