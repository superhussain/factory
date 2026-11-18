import { type PluginClientContext, type PluginSidebarItemProps } from "@getpaseo/plugin/client";
import { SidebarRow } from "@getpaseo/plugin/client/ui";
import { useSyncExternalStore } from "react";
import { InboxBadge } from "./client/inbox-badge";
import { dashboardScreen } from "./client/dashboard-screen";
import { SettingsScreen } from "./client/settings-screen";
import { showNeedsNotification } from "./client/web";
import { createInbox, type Inbox } from "./shared/arrivals";
import { factoryInbox } from "./shared/inbox";

function factoryItem(inbox: Inbox) {
  return function FactoryItem({ currentScreen, openScreen, theme }: PluginSidebarItemProps) {
    const count = useSyncExternalStore(inbox.listen, inbox.get);
    return (
      <SidebarRow
        icon="Factory"
        active={currentScreen?.screenId === "dashboard"}
        onPress={() => openScreen({ screenId: "dashboard" })}
        trailing={count > 0 ? <InboxBadge count={count} theme={theme} /> : undefined}
      />
    );
  };
}

export default function contribute(client: PluginClientContext) {
  const inbox = createInbox((job) =>
    showNeedsNotification(job, () =>
      client.openScreen({ screenId: "dashboard", params: { job: job.id } }),
    ),
  );
  const subscription = client.subscribe(factoryInbox, {}, inbox.apply);
  client.addScreen({ id: "dashboard", title: "Factory", Component: dashboardScreen(client) });
  client.addSidebarHeaderItem({ id: "factory", title: "Factory", Component: factoryItem(inbox) });
  client.addSettingsScreen({
    id: "dashboard",
    title: "Dashboard",
    icon: "Settings",
    Component: SettingsScreen,
  });
  return () => subscription.release();
}
