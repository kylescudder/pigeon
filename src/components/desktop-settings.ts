import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import {
  desktopSettings,
  isDesktop,
  setDesktopSettings,
} from "../lib/native";

const DESKTOP_SETTINGS_KEY = ["desktop", "settings"] as const;

export function useDesktopSettings() {
  return useQuery({
    queryKey: DESKTOP_SETTINGS_KEY,
    queryFn: desktopSettings,
    enabled: isDesktop(),
  });
}

export function useSaveDesktopSettings() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: setDesktopSettings,
    onSuccess: (saved) => client.setQueryData(DESKTOP_SETTINGS_KEY, saved),
  });
}
