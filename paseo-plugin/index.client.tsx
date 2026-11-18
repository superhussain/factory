import type { PluginClientContext, PluginSidebarItemProps } from "@getpaseo/plugin/client";
import { SidebarRow } from "@getpaseo/plugin/client/ui";
import { dashboardScreen } from "./client/dashboard-screen";
import { SettingsScreen } from "./client/settings-screen";

function FactoryItem({ currentScreen, openScreen }: PluginSidebarItemProps) {
  return (
    <SidebarRow
      icon="Factory"
      active={currentScreen?.screenId === "dashboard"}
      onPress={() => openScreen({ screenId: "dashboard" })}
    />
  );
}

export default function contribute(client: PluginClientContext) {
  client.addScreen({ id: "dashboard", title: "Factory", Component: dashboardScreen(client) });
  client.addSidebarHeaderItem({ id: "factory", title: "Factory", Component: FactoryItem });
  client.addSettingsScreen({
    id: "dashboard",
    title: "Dashboard",
    icon: "Settings",
    Component: SettingsScreen,
  });
  return () => {};
}
