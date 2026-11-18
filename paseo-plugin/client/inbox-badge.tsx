import type { PluginTheme } from "@getpaseo/plugin";
import { Text, View } from "react-native";
import { badgeLabel, badgeText } from "../shared/badge";

export function InboxBadge({ count, theme }: { count: number; theme: PluginTheme }) {
  return (
    <View
      accessible
      accessibilityLabel={badgeLabel(count)}
      style={{
        minWidth: 20,
        height: 20,
        borderRadius: 999,
        paddingHorizontal: 6,
        alignItems: "center",
        justifyContent: "center",
        backgroundColor: theme.colors.accent,
      }}
    >
      <Text style={{ color: theme.colors.accentForeground, fontSize: 12, fontWeight: "600" }}>
        {badgeText(count)}
      </Text>
    </View>
  );
}
