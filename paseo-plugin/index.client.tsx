import { type PluginClientContext, type PluginSidebarItemProps } from "@getpaseo/plugin/client";
import { SidebarRow } from "@getpaseo/plugin/client/ui";
import { useSyncExternalStore } from "react";
import { InboxBadge } from "./client/inbox-badge";
import { dashboardScreen } from "./client/dashboard-screen";
import { createDashboardLinks } from "./client/deep-link";
import { startElection } from "./client/election";
import { useFrameInboxCount } from "./client/frame-inbox";
import { settingsScreen } from "./client/settings-screen";
import { dropWarmFrame, showNeedsNotification } from "./client/web";
import { createInbox, type Inbox } from "./shared/arrivals";
import { shownInboxCount } from "./shared/badge";
import { setScreenOpener } from "./shared/deep-link";
import { dashboardHost } from "./shared/embed";
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
  const inbox = createInbox((job) => {
    if (!election.elected()) return;
    showNeedsNotification(job, () =>
      client.openScreen({ screenId: "dashboard", params: { job: job.id } }),
    );
  });
  const links = createDashboardLinks(client);
  const election = startElection(
    client,
    (origin) => {
      const removeScreen = client.addScreen({
        id: "dashboard",
        title: origin ? dashboardHost(origin) : "Factory",
        Component: dashboardScreen(client, reload),
      });
      const removeItem = client.addSidebarHeaderItem({
        id: "factory",
        title: "Factory",
        Component: factoryItem(inbox, reload),
      });
      const removeOpener = origin
        ? setScreenOpener(globalThis, origin, (hash) =>
            client.openScreen({ screenId: "dashboard", params: { hash, at: String(Date.now()) } }),
          )
        : () => {};
      return () => {
        removeOpener();
        dropWarmFrame();
        void removeItem();
        void removeScreen();
      };
    },
    links.update,
  );
  const subscription = client.subscribe(factoryInbox, {}, inbox.apply);
  client.addSettingsScreen({
    id: "dashboard",
    title: "Dashboard",
    icon: "Settings",
    Component: settingsScreen(() => election.recheck()),
  });
  return () => {
    election.stop();
    links.stop();
    subscription.release();
    dropWarmFrame();
  };
}
