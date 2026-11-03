import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { senderImage } from "../lib/native";
import { describeSender } from "../lib/sender";
import { useDesktopSettings } from "./desktop-settings";

/** Decorative: whatever sits beside it already names the sender. */
export function SenderAvatar({ sender }: { sender: string }) {
  const { initials, tone, domain } = describeSender(sender);
  const settings = useDesktopSettings();
  const wanted = !!settings.data?.settings.senderImages && domain !== null;
  const icon = useQuery({
    queryKey: ["sender-image", domain],
    queryFn: () => senderImage(domain as string),
    enabled: wanted,
    // The store keeps these for days. A failed lookup waits for a later visit.
    staleTime: 60 * 60 * 1000,
    retryOnMount: false,
  });
  const [broken, setBroken] = useState<string | null>(null);
  const image = wanted && icon.data !== broken ? icon.data : null;
  return (
    <span
      className={`sender-avatar sender-tone-${tone}${image ? " sender-avatar-image" : ""}`}
      aria-hidden="true"
    >
      {image ? (
        <img src={image} alt="" onError={() => setBroken(image)} />
      ) : (
        initials
      )}
    </span>
  );
}
