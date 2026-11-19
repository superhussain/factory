import { type PluginClientContext, type PluginSidebarItemProps } from "@getpaseo/plugin/client";
import { SidebarRow } from "@getpaseo/plugin/client/ui";
import { useSyncExternalStore } from "react";
import { InboxBadge } from "./client/inbox-badge";
import { dashboardScreen } from "./client/dashboard-screen";
import { useFrameInboxCount } from "./client/frame-inbox";
import { SettingsScreen } from "./client/settings-screen";
import { dropWarmFrame, showNeedsNotification } from "./client/web";
import { createInbox, type Inbox } from "./shared/arrivals";
import { shownInboxCount } from "./shared/badge";
import { factoryInbox } from "./shared/inbox";
import { createReload, type Reload } from "./shared/reload";

function factoryItem(inbox: Inbox, reload: Reload) {
  return function FactoryItem({ currentScreen, openScreen, theme }: PluginSidebarItemProps) {
    const count = useSyncExternalStore(inbox.listen, inbox.get);
    const frame = useFrameInboxCount();
    const active = currentScreen?.screenId === "dashboard";
    const shown = shownInboxCount(count, frame, active);
    return (
      <SidebarRow
        icon="Factory"
        active={active}
        onPress={() => (active ? reload.fire() : openScreen({ screenId: "dashboard" }))}
        trailing={shown > 0 ? <InboxBadge count={shown} theme={theme} /> : undefined}
      />
    );
  };
}

export default function contribute(client: PluginClientContext) {
  const reload = createReload();
  const inbox = createInbox((job) =>
    showNeedsNotification(job, () =>
      client.openScreen({ screenId: "dashboard", params: { job: job.id } }),
    ),
  );
  const subscription = client.subscribe(factoryInbox, {}, inbox.apply);
  client.addScreen({
    id: "dashboard",
    title: "Factory",
    Component: dashboardScreen(client, reload),
  });
  client.addSidebarHeaderItem({
    id: "factory",
    title: "Factory",
    Component: factoryItem(inbox, reload),
  });
  client.addSettingsScreen({
    id: "dashboard",
    title: "Dashboard",
    icon: "Settings",
    Component: SettingsScreen,
  });
  return () => {
    subscription.release();
    dropWarmFrame();
  };
}
