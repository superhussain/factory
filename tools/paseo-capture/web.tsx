// Capture stub for paseo-plugin/client/web.ts: no frame, and the card picked by
// globalThis.FACTORY_CAPTURE_CARD, which tools/capture-plugin-cards.mjs sets.
const card = globalThis.FACTORY_CAPTURE_CARD;

export const savedLocation = () => "";
export const dropWarmFrame = () => {};
export const frameReady = () => false;
export const cardHeldFocus = () => false;
export const focusFrame = () => {};
export const frameSignedOut = () => card === "expired";
export const hasLoadedBefore = () => card === "stall";
export const paseoOrigin = () => "https://app.example.test";
export const DashboardFrame = () => null;
export const useSignInPopup = () => () => {};
