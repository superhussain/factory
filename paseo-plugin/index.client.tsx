import {
  type PluginClientContext,
  type PluginSidebarItemProps,
  useSubscription,
} from "@getpaseo/plugin/client";
import { SidebarRow } from "@getpaseo/plugin/client/ui";
import { useState } from "react";
import { InboxBadge } from "./client/inbox-badge";
import { dashboardScreen } from "./client/dashboard-screen";
import { SettingsScreen } from "./client/settings-screen";
import { factoryInbox } from "./shared/inbox";

const NO_INPUT = {};

function FactoryItem({ currentScreen, openScreen, theme }: PluginSidebarItemProps) {
  const [count, setCount] = useState(0);
  useSubscription(factoryInbox, NO_INPUT, (event) => setCount(event.count));
  return (
    <SidebarRow
      icon="Factory"
      active={currentScreen?.screenId === "dashboard"}
      onPress={() => openScreen({ screenId: "dashboard" })}
      trailing={count > 0 ? <InboxBadge count={count} theme={theme} /> : undefined}
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
