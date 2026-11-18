import {
  type PluginClientContext,
  type PluginSidebarItemProps,
  useSubscription,
} from "@getpaseo/plugin/client";
import { SidebarRow } from "@getpaseo/plugin/client/ui";
import { useState } from "react";
import { InboxBadge } from "./client/inbox-badge";
import { dashboardScreen } from "./client/dashboard-screen";
import { useFrameInboxCount } from "./client/frame-inbox";
import { SettingsScreen } from "./client/settings-screen";
import { shownInboxCount } from "./shared/badge";
import { factoryInbox } from "./shared/inbox";

const NO_INPUT = {};

function FactoryItem({ currentScreen, openScreen, theme }: PluginSidebarItemProps) {
  const [count, setCount] = useState(0);
  const frame = useFrameInboxCount();
  useSubscription(factoryInbox, NO_INPUT, (event) => setCount(event.count));
  const active = currentScreen?.screenId === "dashboard";
  const shown = shownInboxCount(count, frame, active);
  return (
    <SidebarRow
      icon="Factory"
      active={active}
      onPress={() => openScreen({ screenId: "dashboard" })}
      trailing={shown > 0 ? <InboxBadge count={shown} theme={theme} /> : undefined}
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
