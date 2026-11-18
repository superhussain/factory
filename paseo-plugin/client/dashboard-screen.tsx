import type { PluginClientContext, PluginScreenProps } from "@getpaseo/plugin/client";
import { openExternalUrl, useSettings } from "@getpaseo/plugin/client";
import { ExternalLink, Loading, useToast } from "@getpaseo/plugin/client/ui";
import { useEffect, useState } from "react";
import { Platform, Pressable, Text, View } from "react-native";
import { dashboardSrc, parseDashboardOrigin, type EmbedMessage } from "../shared/embed";
import { factorySettings } from "../shared/settings";
import { DashboardFrame } from "./web";

const READY_TIMEOUT_MS = 8000;

export function dashboardScreen(client: PluginClientContext) {
  return function DashboardScreen({ theme, layout, params, navigation }: PluginScreenProps) {
    const settings = useSettings(factorySettings);
    const toast = useToast();
    const [load, setLoad] = useState(0);
    const [ready, setReady] = useState(false);
    const [stalled, setStalled] = useState(false);
    const origin = settings.status === "ready" ? parseDashboardOrigin(settings.values.origin) : null;
    useEffect(() => {
      if (!origin || Platform.OS !== "web") return;
      setReady(false);
      setStalled(false);
      const timer = setTimeout(() => setStalled(true), READY_TIMEOUT_MS);
      return () => clearTimeout(timer);
    }, [origin, load]);

    const styles = {
      screen: { flex: 1, backgroundColor: theme.colors.surface0 },
      body: { padding: layout.compact ? 16 : 24, gap: 12 },
      text: { color: theme.colors.foreground },
      muted: { color: theme.colors.foregroundMuted },
      bar: {
        flexDirection: "row" as const,
        flexWrap: "wrap" as const,
        alignItems: "center" as const,
        gap: 12,
        padding: layout.compact ? 16 : 24,
        borderBottomWidth: 1,
        borderColor: theme.colors.border,
      },
      button: {
        alignSelf: "flex-start" as const,
        paddingVertical: 8,
        paddingHorizontal: 12,
        backgroundColor: theme.colors.accent,
      },
      buttonText: { color: theme.colors.accentForeground },
    };
    const button = (label: string, onPress: () => void) => (
      <Pressable accessibilityRole="button" style={styles.button} onPress={onPress}>
        <Text style={styles.buttonText}>{label}</Text>
      </Pressable>
    );

    if (settings.status === "loading") return <Loading />;
    if (!origin)
      return (
        <View style={[styles.screen, styles.body]}>
          <Text style={styles.text}>Set your factory dashboard URL to show it here.</Text>
          {button("Open settings", () => client.openSettings("dashboard"))}
        </View>
      );
    const src = dashboardSrc(origin, params.job);
    if (Platform.OS !== "web")
      return (
        <View style={[styles.screen, styles.body]}>
          <Text style={styles.text}>Your factory dashboard opens in the browser on this device.</Text>
          <ExternalLink href={src}>Open factory dashboard</ExternalLink>
        </View>
      );

    const onMessage = (message: EmbedMessage) => {
      if (message.type === "factory-embed-ready") {
        setReady(true);
        setStalled(false);
      } else if (navigation)
        navigation.openAgent({ agentId: message.agent, serverId: message.server });
      else toast.error("This Paseo app cannot open agents from plugins.");
    };
    return (
      <View style={styles.screen}>
        {stalled && !ready ? (
          <View style={styles.bar}>
            <Text style={styles.text}>
              Factory did not load. Sign in through Cloudflare Access, then reload.
            </Text>
            {button("Sign in", () => void openExternalUrl(origin))}
            {button("Reload", () => setLoad((n) => n + 1))}
          </View>
        ) : null}
        <View style={{ flex: 1 }}>
          <DashboardFrame key={load} src={src} origin={origin} onMessage={onMessage} />
        </View>
      </View>
    );
  };
}
