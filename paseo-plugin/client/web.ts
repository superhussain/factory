import { openExternalUrl } from "@getpaseo/plugin/client";
import { createElement, useEffect, useLayoutEffect, useRef } from "react";
import { Platform } from "react-native";
import {
  dashboardSrc,
  frameLoaded,
  readEmbedMessage,
  readSignedInMessage,
  type EmbedMessage,
} from "../shared/embed";
import { needsBody, needsTitle, type WaitingJob } from "../shared/arrivals";
import * as firstLogin from "../shared/first-login";
import { readLocation, writeLocation } from "../shared/location";
import {
  createFrameKeeper,
  shouldFocusFrame,
  type InputKind,
  type KeptFrame,
} from "../shared/warm-frame";
import { setFrameInboxCount } from "./frame-inbox";

// This plugin typechecks without the DOM library. Declare only what this module uses.
type MessageSource = { postMessage(message: unknown, targetOrigin: string): void };
type FrameMessage = { source: unknown; origin: string; data: unknown };
type SignInWindow = { closed: boolean; close(): void; focus(): void };
type FrameElement = {
  src: string;
  title: string;
  style: { cssText: string };
  setAttribute(name: string, value: string): void;
  addEventListener(type: "load", listener: () => void): void;
  remove(): void;
  contentWindow: MessageSource | null;
  clientWidth: number;
  clientHeight: number;
  focus(): void;
};
type DivElement = {
  id: string;
  style: { cssText: string; width: string; height: string };
  setAttribute(name: string, value: string): void;
  appendChild(node: FrameElement): unknown;
  moveBefore?: (node: FrameElement, child: null) => void;
};
declare const document: {
  createElement(tag: "iframe"): FrameElement;
  createElement(tag: "div"): DivElement;
  getElementById(id: string): DivElement | null;
  body: { appendChild(node: DivElement): unknown };
  activeElement: { tagName: string; isContentEditable: boolean } | null;
};
declare const window: {
  focus(): void;
  open(url: string, target: string, features: string): SignInWindow | null;
  addEventListener(type: "message", listener: (event: FrameMessage) => void): void;
  addEventListener(type: "focus", listener: () => void): void;
  addEventListener(type: "pointerdown" | "keydown", listener: () => void, capture: boolean): void;
  removeEventListener(type: "message", listener: (event: FrameMessage) => void): void;
  removeEventListener(type: "focus", listener: () => void): void;
  removeEventListener(
    type: "pointerdown" | "keydown",
    listener: () => void,
    capture: boolean,
  ): void;
};
declare const localStorage: {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
};
declare const sessionStorage: firstLogin.SeenStorage;
declare const location: { origin: string };
type Permission = "granted" | "denied" | "default";
declare const Notification: {
  readonly permission: Permission;
  requestPermission(): Promise<Permission>;
  new (
    title: string,
    options: { body: string; tag: string },
  ): {
    onclick: (() => void) | null;
    close(): void;
  };
};

const WEB: firstLogin.WebGlobals = { storage: () => localStorage, origin: () => location.origin };

const SESSION = () => sessionStorage;

export function savedLocation(origin: string): string {
  return readLocation(Platform.OS === "web" ? SESSION : null, origin);
}

export function saveLocation(origin: string, hash: string): void {
  writeLocation(Platform.OS === "web" ? SESSION : null, origin, hash);
}

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

const PARK_ID = "factory-plugin-warm-park";

const keeper = createFrameKeeper<FrameElement, EmbedMessage>({
  create(origin, src) {
    const element = document.createElement("iframe");
    element.title = "Factory dashboard";
    element.setAttribute("sandbox", SANDBOX);
    element.style.cssText = "border:0;width:100%;height:100%;display:block";
    element.src = src;
    element.addEventListener("load", () => {
      // A new document must post ready again, so a parked frame that reloaded shows the stall card on return.
      const kept = keeper.current();
      if (kept?.element === element) kept.ready = false;
      frameLoaded(
        element.contentWindow,
        origin,
        setFrameInboxCount,
        !firstLogin.isBrowserOrigin(paseoOrigin()),
      );
    });
    listen();
    return element;
  },
  park(element) {
    let park = document.getElementById(PARK_ID);
    if (!park) {
      park = document.createElement("div");
      park.id = PARK_ID;
      park.setAttribute("inert", "");
      park.setAttribute("aria-hidden", "true");
      park.style.cssText =
        "position:fixed;left:-100000px;top:0;visibility:hidden;pointer-events:none";
      document.body.appendChild(park);
    }
    park.style.width = element.clientWidth + "px";
    park.style.height = element.clientHeight + "px";
    return park;
  },
  dropped() {
    setFrameInboxCount(null);
  },
});

let listening = false;
// The last input in Paseo's own document, so a warm return can tell a click on the Factory row from a key press.
let lastInput: InputKind | null = null;
let pressed: InputKind | null = null;
const onPointer = () => {
  lastInput = "pointer";
};
const onKey = () => {
  lastInput = "keyboard";
};

export function noteSidebarOpen(): void {
  pressed = lastInput;
}

function listen(): void {
  if (listening) return;
  listening = true;
  window.addEventListener("message", route);
  window.addEventListener("pointerdown", onPointer, true);
  window.addEventListener("keydown", onKey, true);
}

// One router for the kept frame, attached or parked, so a parked page keeps its saved page and count current.
function route(event: FrameMessage): void {
  const frame = keeper.current();
  if (!frame || event.source !== frame.element.contentWindow || event.origin !== frame.origin)
    return;
  const message = readEmbedMessage(event.data);
  if (!message) return;
  if (message.type === "factory-inbox-count") setFrameInboxCount(message.count);
  else if (message.type === "factory-embed-location") saveLocation(frame.origin, message.hash);
  else if (message.type === "factory-embed-ready") {
    frame.ready = true;
    markLoaded(frame.origin);
  }
  frame.onMessage?.(message);
}

export function dropWarmFrame(): void {
  keeper.drop();
  window.removeEventListener("message", route);
  window.removeEventListener("pointerdown", onPointer, true);
  window.removeEventListener("keydown", onKey, true);
  listening = false;
  lastInput = null;
  pressed = null;
}

export function frameReady(origin: string): boolean {
  const frame = keeper.current();
  return !!frame && frame.origin === origin && frame.parkedAt === null && frame.ready;
}

export function DashboardFrame({
  src,
  origin,
  jump,
  jumpKey,
  onMessage,
}: {
  src: string;
  origin: string;
  jump: string | null;
  jumpKey: string;
  onMessage(message: EmbedMessage): void;
}) {
  const host = useRef<DivElement | null>(null);
  const attached = useRef<KeptFrame<FrameElement, EmbedMessage> | null>(null);
  const jumped = useRef<string | undefined>(undefined);
  const handler = useRef(onMessage);
  handler.current = onMessage;
  useLayoutEffect(() => {
    const node = host.current;
    if (Platform.OS !== "web" || !node) return;
    const { frame, warm } = keeper.attach(node, origin, src);
    frame.onMessage = (message) => handler.current(message);
    if (warm && jump) keeper.jumpTo(frame, jump);
    jumped.current = jumpKey;
    attached.current = frame;
    const press = pressed;
    pressed = null;
    if (shouldFocusFrame(warm, press, document.activeElement)) frame.element.focus();
    return () => {
      attached.current = null;
      keeper.park(frame);
    };
  }, [origin]);
  useEffect(() => {
    const frame = attached.current;
    if (!frame || jumpKey === jumped.current) return;
    jumped.current = jumpKey;
    if (jump) keeper.jumpTo(frame, jump);
  }, [jumpKey]);
  if (Platform.OS !== "web") return null;
  return createElement("div", { ref: host, style: { width: "100%", height: "100%" } });
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

export type NotificationState = Permission | "unsupported";

export function notificationState(): NotificationState {
  if (Platform.OS !== "web" || typeof Notification === "undefined") return "unsupported";
  return Notification.permission;
}

export function onWindowFocus(listener: () => void): () => void {
  if (Platform.OS !== "web") return () => {};
  window.addEventListener("focus", listener);
  return () => window.removeEventListener("focus", listener);
}

export async function requestNotifications(): Promise<NotificationState> {
  if (notificationState() === "unsupported") return "unsupported";
  return Notification.requestPermission();
}

export function showNeedsNotification(job: WaitingJob, onClick: () => void): void {
  if (notificationState() !== "granted") return;
  const notification = new Notification(needsTitle(job), {
    body: needsBody(job),
    tag: "factory-needs-" + job.id,
  });
  notification.onclick = () => {
    window.focus();
    onClick();
    notification.close();
  };
}
