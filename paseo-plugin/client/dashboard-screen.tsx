import type { PluginClientContext, PluginScreenProps } from "@getpaseo/plugin/client";
import { openExternalUrl, useSettings } from "@getpaseo/plugin/client";
import { ExternalLink, Icon, Loading, ScrollView, useToast } from "@getpaseo/plugin/client/ui";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { Platform, Pressable, Text, View } from "react-native";
import { parseDashboardOrigin, type EmbedMessage } from "../shared/embed";
import { frameUrl, jumpUrl, startUrl } from "../shared/location";
import type { Reload } from "../shared/reload";
import { firstLoginSteps, isBrowserOrigin, openLabel } from "../shared/first-login";
import { createOpenLimiter } from "../shared/open-limit";
import { factorySettings } from "../shared/settings";
import { CARD_ID } from "../shared/warm-frame";
import {
  cardHeldFocus,
  DashboardFrame,
  dropWarmFrame,
  focusFrame,
  frameReady,
  frameSignedOut,
  hasLoadedBefore,
  paseoOrigin,
  savedLocation,
  useSignInPopup,
} from "./web";

const READY_TIMEOUT_MS = 4000;

export function dashboardScreen(client: PluginClientContext, reload: Reload) {
  return function DashboardScreen({ theme, layout, params, navigation }: PluginScreenProps) {
    const settings = useSettings(factorySettings);
    const toast = useToast();
    const [load, setLoad] = useState(0);
    const [ready, setReady] = useState(false);
    const [stalled, setStalled] = useState(false);
    const [expired, setExpired] = useState(false);
    const origin =
      settings.status === "ready" ? parseDashboardOrigin(settings.values.origin) : null;
    const [saved, setSaved] = useState<string | null>(() =>
      origin ? startUrl(origin, params.job, savedLocation(origin), params.hash) : null,
    );
    useEffect(() => {
      if (origin) setSaved(startUrl(origin, params.job, savedLocation(origin), params.hash));
    }, [origin, params.job, params.hash, params.at]);
    const src = origin && saved?.startsWith(origin + "/") ? saved : null;
    const reloadFrame = () => {
      dropWarmFrame();
      if (origin) setSaved(frameUrl(origin, savedLocation(origin)));
      setLoad((n) => n + 1);
    };
    const reloadRef = useRef(reloadFrame);
    reloadRef.current = reloadFrame;
    useEffect(() => reload.listen(() => reloadRef.current()), []);
    const [allowOpen] = useState(() => createOpenLimiter());
    const signIn = useSignInPopup(origin ?? "", reloadFrame);
    const showing = src !== null;
    useLayoutEffect(() => {
      if (!origin || !showing || Platform.OS !== "web") return;
      if (frameReady(origin)) {
        setReady(true);
        setStalled(false);
        setExpired(false);
        return;
      }
      if (frameSignedOut(origin)) {
        setReady(false);
        setStalled(true);
        setExpired(true);
        return;
      }
      setReady(false);
      setStalled(false);
      setExpired(false);
      const timer = setTimeout(() => setStalled(true), READY_TIMEOUT_MS);
      return () => clearTimeout(timer);
    }, [origin, load, showing]);
    const refocus = useRef(false);
    useEffect(() => {
      if (!refocus.current) return;
      refocus.current = false;
      if (origin) focusFrame(origin);
    }, [ready]);

    const styles = {
      screen: { flex: 1, backgroundColor: theme.colors.surface0 },
      body: { padding: layout.compact ? 16 : 24, gap: 12 },
      text: { color: theme.colors.foreground },
      muted: { color: theme.colors.foregroundMuted },
      overlay: {
        position: "absolute" as const,
        top: 0,
        left: 0,
        right: 0,
        bottom: 0,
        backgroundColor: theme.colors.surface0,
      },
      overlayContent: {
        flexGrow: 1,
        alignItems: "center" as const,
        justifyContent: "center" as const,
        padding: layout.compact ? 16 : 24,
      },
      stepLabel: { color: theme.colors.foregroundMuted, fontSize: 12, fontWeight: "600" as const },
      stepText: { color: theme.colors.foreground, flex: 1, lineHeight: 22 },
      body22: { color: theme.colors.foregroundMuted, lineHeight: 22 },
      footnote: { color: theme.colors.foregroundMuted, fontSize: 12, lineHeight: 18 },
      card: {
        width: "100%" as const,
        maxWidth: 520,
        gap: 16,
        padding: layout.compact ? 20 : 28,
        borderRadius: 16,
        borderWidth: 1,
        borderColor: theme.colors.border,
        backgroundColor: theme.colors.surface1,
      },
      title: { color: theme.colors.foreground, fontSize: 18, fontWeight: "600" as const },
      step: { flexDirection: "row" as const, gap: 12, alignItems: "flex-start" as const },
      stepNumber: {
        width: 24,
        height: 24,
        borderRadius: 12,
        alignItems: "center" as const,
        justifyContent: "center" as const,
        backgroundColor: theme.colors.surface2,
      },
      actions: { flexDirection: "row" as const, flexWrap: "wrap" as const, gap: 12 },
      button: {
        flexDirection: "row" as const,
        alignItems: "center" as const,
        justifyContent: "center" as const,
        gap: 8,
        minHeight: 44,
        borderRadius: 999,
        paddingVertical: 10,
        paddingHorizontal: 18,
        backgroundColor: theme.colors.accent,
      },
      secondary: {
        backgroundColor: "transparent",
        borderWidth: 1,
        borderColor: theme.colors.border,
      },
      buttonText: { color: theme.colors.accentForeground, fontWeight: "600" as const },
      secondaryText: { color: theme.colors.foreground, fontWeight: "600" as const },
    };
    const button = (label: string, onPress: () => void, icon?: string, secondary?: boolean) => (
      <Pressable
        accessibilityRole="button"
        style={[styles.button, secondary ? styles.secondary : null]}
        onPress={onPress}
      >
        {icon ? (
          <Icon
            name={icon}
            size={16}
            color={secondary ? theme.colors.foreground : theme.colors.accentForeground}
          />
        ) : null}
        <Text style={secondary ? styles.secondaryText : styles.buttonText}>{label}</Text>
      </Pressable>
    );
    const step = (n: number, text: string) => (
      <View key={n} style={styles.step}>
        <View style={styles.stepNumber}>
          <Text style={styles.stepLabel}>{n}</Text>
        </View>
        <Text style={styles.stepText}>{text}</Text>
      </View>
    );

    if (settings.status === "loading") return <Loading />;
    if (!origin)
      return (
        <View style={[styles.screen, styles.body]}>
          <Text style={styles.text}>Set your factory dashboard URL to show it here.</Text>
          {button("Open settings", () => client.openSettings("dashboard"))}
        </View>
      );
    if (!src) return <Loading />;
    if (Platform.OS !== "web")
      return (
        <View style={[styles.screen, styles.body]}>
          <Text style={styles.text}>
            Your factory dashboard opens in the browser on this device.
          </Text>
          <ExternalLink href={src}>Open factory dashboard</ExternalLink>
        </View>
      );

    const onMessage = (message: EmbedMessage) => {
      if (message.type === "factory-embed-ready") {
        refocus.current = cardHeldFocus();
        setReady(true);
        setStalled(false);
        setExpired(false);
      } else if (message.type === "factory-embed-signin") {
        setReady(false);
        setStalled(true);
        setExpired(true);
      } else if (message.type === "factory-open-url") {
        if (allowOpen())
          openExternalUrl(message.url).catch(() => toast.error("Paseo could not open that link."));
      } else if (message.type === "factory-open-agent") {
        if (navigation) navigation.openAgent({ agentId: message.agent, serverId: message.server });
        else toast.error("This Paseo app cannot open agents from plugins.");
      }
    };
    const returning = hasLoadedBefore(origin);
    const here = paseoOrigin();
    const browser = isBrowserOrigin(here);
    return (
      <View style={styles.screen}>
        <View style={{ flex: 1 }}>
          <DashboardFrame
            key={load}
            src={src}
            origin={origin}
            jump={jumpUrl(origin, params.job, params.hash)}
            jumpKey={`${params.job ?? ""}|${params.hash ?? ""}|${params.at ?? ""}`}
            onMessage={onMessage}
          />
        </View>
        {ready ? null : (
          <ScrollView style={styles.overlay} contentContainerStyle={styles.overlayContent}>
            {stalled ? (
              <View nativeID={CARD_ID} style={styles.card}>
                <Icon name="Factory" size={28} color={theme.colors.accent} />
                <Text style={styles.title}>
                  {expired
                    ? "Your sign-in has expired"
                    : returning
                      ? "Factory did not load"
                      : "Sign in to your factory"}
                </Text>
                <Text style={styles.body22}>
                  {expired
                    ? "Sign in again to keep using your board here."
                    : returning
                      ? "Your session may have ended, or the dashboard is unreachable. Sign in again, or reload once you are back online."
                      : `Paseo shows your factory dashboard from ${origin.replace("https://", "")} right here. It sits behind Cloudflare Access, so this device needs to sign in once.`}
                </Text>
                {returning || expired ? null : (
                  <View style={{ gap: 12 }}>
                    {firstLoginSteps(browser).map((text, i) => step(i + 1, text))}
                  </View>
                )}
                <View style={styles.actions}>
                  {button("Sign in", signIn, "LogIn")}
                  {button("Reload", reloadFrame, "RotateCw", true)}
                  {button(
                    openLabel(browser),
                    () => void openExternalUrl(src),
                    "ExternalLink",
                    true,
                  )}
                </View>
                {expired ? null : (
                  <Text style={styles.footnote}>
                    {`Signed in and still blank? Ask your factory operator to add ${here ?? "this Paseo origin"} to embed_origins.`}
                  </Text>
                )}
                {browser && returning && !expired ? (
                  <Text style={styles.footnote}>
                    Using Safari, or another browser that blocks third-party cookies? The board
                    cannot load here. Select Open in a browser tab.
                  </Text>
                ) : null}
              </View>
            ) : (
              <View style={{ alignItems: "center", gap: 12 }}>
                <Loading />
                <Text style={styles.muted}>Connecting to your factory</Text>
              </View>
            )}
          </ScrollView>
        )}
      </View>
    );
  };
}
