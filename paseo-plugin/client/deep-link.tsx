import type { PluginClientContext, PluginTimelineItemProps } from "@getpaseo/plugin/client";
import { openExternalUrl } from "@getpaseo/plugin/client";
import { Icon, Markdown } from "@getpaseo/plugin/client/ui";
import { Pressable, Text, View } from "react-native";
import { z } from "zod";
import {
  createLinkSwitch,
  findDashboardLinks,
  MAX_DASHBOARD_LINKS,
  screenOpeners,
  type DashboardLink,
} from "../shared/deep-link";

const KIND = "dashboard-links";

const schema = z.object({
  text: z.string(),
  phase: z.enum(["streaming", "complete"]),
  links: z
    .array(z.object({ url: z.string(), origin: z.string(), hash: z.string(), label: z.string() }))
    .max(MAX_DASHBOARD_LINKS),
});

function open(link: DashboardLink) {
  const opener = screenOpeners(globalThis).get(link.origin);
  if (opener) opener(link.hash);
  else void openExternalUrl(link.url);
}

function DashboardLinksReply({ item, theme }: PluginTimelineItemProps<z.infer<typeof schema>>) {
  const { text, phase, links } = item.data;
  return (
    <View>
      <Markdown>{text}</Markdown>
      {phase === "complete" ? (
        <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
          {links.map((link) => (
            <Pressable
              key={link.url}
              accessibilityRole="link"
              onPress={() => open(link)}
              style={{
                flexDirection: "row",
                alignItems: "center",
                gap: 6,
                borderRadius: 999,
                borderWidth: 1,
                borderColor: theme.colors.border,
                backgroundColor: theme.colors.surface1,
                paddingVertical: 6,
                paddingHorizontal: 12,
              }}
            >
              <Icon name="Factory" size={14} color={theme.colors.foreground} />
              <Text
                style={{ color: theme.colors.foreground }}
              >{`Open ${link.label} in Factory`}</Text>
            </Pressable>
          ))}
        </View>
      ) : null}
    </View>
  );
}

// Off until the owner turns the switch on, then registered for this installation's origin.
export function createDashboardLinks(client: PluginClientContext) {
  return createLinkSwitch((origin) => {
    const removeRenderer = client.addTimelineRenderer({
      kind: KIND,
      version: 1,
      schema,
      Component: DashboardLinksReply,
    });
    const removeTransformer = client.addTimelineTransformer({
      id: KIND,
      query: { itemType: "assistant_message" },
      transform({ item, phase }) {
        const links = findDashboardLinks(item.text, origin);
        if (links.length === 0) return undefined;
        return {
          items: [
            { type: "plugin", kind: KIND, version: 1, data: { text: item.text, phase, links } },
          ],
        };
      },
    });
    return () => {
      void removeTransformer();
      void removeRenderer();
    };
  });
}
