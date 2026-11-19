// One warm dashboard frame per plugin installation. It parks while the Factory screen is away
// and is dropped after WARM_FRAME_MS. Only Element.moveBefore keeps a moved iframe's document;
// appendChild reloads it, so without moveBefore the frame is dropped instead of parked.
export const WARM_FRAME_MS = 4 * 60 * 60_000;

export type FrameNode = { src: string; remove(): void };
export type FrameParent<E> = {
  appendChild(node: E): unknown;
  moveBefore?: (node: E, child: null) => void;
};
export type KeeperDom<E> = {
  create(origin: string, src: string): E;
  park(element: E): FrameParent<E>;
  dropped(element: E): void;
};
export type KeptFrame<E, M> = {
  origin: string;
  element: E;
  ready: boolean;
  parkedAt: number | null;
  onMessage: ((message: M) => void) | null;
};

export function createFrameKeeper<E extends FrameNode, M>(
  dom: KeeperDom<E>,
  now: () => number = Date.now,
) {
  let kept: KeptFrame<E, M> | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  const stopTimer = () => {
    if (timer !== null) clearTimeout(timer);
    timer = null;
  };
  const move = (parent: FrameParent<E>, element: E): boolean => {
    if (typeof parent.moveBefore !== "function") return false;
    try {
      parent.moveBefore(element, null);
      return true;
    } catch {
      return false;
    }
  };
  function drop(): void {
    stopTimer();
    if (!kept) return;
    const { element } = kept;
    kept = null;
    element.remove();
    dom.dropped(element);
  }
  function attach(host: FrameParent<E>, origin: string, src: string) {
    if (
      kept &&
      (kept.origin !== origin || kept.parkedAt === null || now() - kept.parkedAt >= WARM_FRAME_MS)
    )
      drop();
    if (kept && move(host, kept.element)) {
      stopTimer();
      kept.parkedAt = null;
      return { frame: kept, warm: true };
    }
    drop();
    const element = dom.create(origin, src);
    host.appendChild(element);
    kept = { origin, element, ready: false, parkedAt: null, onMessage: null };
    return { frame: kept, warm: false };
  }
  function park(frame: KeptFrame<E, M>): void {
    if (frame !== kept || frame.parkedAt !== null) return;
    frame.onMessage = null;
    if (!move(dom.park(frame.element), frame.element)) return drop();
    frame.parkedAt = now();
    timer = setTimeout(drop, WARM_FRAME_MS);
  }
  function jumpTo(frame: KeptFrame<E, M>, src: string): void {
    if (frame === kept) frame.element.src = src;
  }
  return { attach, park, drop, jumpTo, current: () => kept };
}

export type InputKind = "pointer" | "keyboard";
export type ActiveElement = { tagName: string; isContentEditable: boolean } | null;

const TYPING_TAGS = new Set(["INPUT", "TEXTAREA", "SELECT"]);

// A warm return focuses the frame only after a pointer press on the Factory row, and never takes focus from a field.
export function shouldFocusFrame(
  warm: boolean,
  press: InputKind | null,
  active: ActiveElement,
): boolean {
  if (!warm || press !== "pointer") return false;
  return !(active && (TYPING_TAGS.has(active.tagName) || active.isContentEditable));
}

// A Factory row press counts for a frame that mounts within PRESS_MS of it.
export const PRESS_MS = 2_000;
export type Press = { kind: InputKind; at: number };

export function freshPress(press: Press | null, now: number): InputKind | null {
  return press && now - press.at < PRESS_MS ? press.kind : null;
}
