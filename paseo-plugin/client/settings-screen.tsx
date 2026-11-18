import type { PluginSurfaceProps } from "@getpaseo/plugin/client";
import { useSettings } from "@getpaseo/plugin/client";
import {
  Loading,
  SettingsAction,
  SettingsCard,
  SettingsInput,
  SettingsSection,
  useToast,
} from "@getpaseo/plugin/client/ui";
import { useState } from "react";
import { Text } from "react-native";
import { parseDashboardOrigin } from "../shared/embed";
import { factorySettings } from "../shared/settings";

export function SettingsScreen({ theme }: PluginSurfaceProps) {
  const settings = useSettings(factorySettings);
  const toast = useToast();
  const [value, setValue] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  if (settings.status === "loading") return <Loading />;
  if (settings.status !== "ready")
    return <Text style={{ color: theme.colors.foregroundMuted }}>{settings.error}</Text>;
  const ready = settings;
  const save = async () => {
    const origin = parseDashboardOrigin(value ?? ready.values.origin);
    if (!origin) {
      setError("Enter an HTTPS origin with no path, like https://factory.example.com.");
      return;
    }
    setError(null);
    if (await ready.save({ origin }, ready.revision))
      toast.show("Factory URL saved", { variant: "success" });
  };
  return (
    <SettingsSection title="Dashboard">
      <SettingsCard>
        <SettingsInput
          label="Factory dashboard URL"
          hint="The HTTPS origin of your factory, like https://factory.example.com."
          placeholder="https://factory.example.com"
          initialValue={ready.values.origin}
          onChangeText={setValue}
          disabled={ready.saving}
          error={error ?? ready.saveError}
        />
        <SettingsAction
          label="Save changes"
          actionLabel="Save"
          disabled={ready.saving}
          onPress={() => void save()}
        />
      </SettingsCard>
    </SettingsSection>
  );
}
