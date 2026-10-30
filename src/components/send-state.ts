import { useQuery, useQueryClient } from "@tanstack/react-query";
import { sendState, type OutboxItem } from "../lib/native";
import { useMail } from "./mail-provider";

export const SEND_STATE_KEY = ["send", "state"] as const;

/** Whether an item is still changing on its own, so worth polling quickly. */
export function inProgress(item: OutboxItem) {
  return (
    item.send.state === "queued" ||
    item.send.state === "sending" ||
    item.sent_copy.state === "waiting" ||
    item.sent_copy.state === "saving"
  );
}

/** Items the user should look at: not sent, or sent without a Sent copy. */
export function needsAttention(item: OutboxItem) {
  return (
    item.send.state === "failed" ||
    item.send.state === "unknown" ||
    item.send.state === "held" ||
    (item.send.state === "queued" && !!item.send.error) ||
    item.sent_copy.state === "failed"
  );
}

export function useSendState() {
  const mail = useMail();
  return useQuery({
    queryKey: SEND_STATE_KEY,
    queryFn: sendState,
    enabled: mail.desktop && !!mail.state,
    refetchInterval: (query) =>
      query.state.data?.outbox.some(inProgress) ? 1000 : 5000,
  });
}

export function useRefreshSend() {
  const client = useQueryClient();
  return () => client.invalidateQueries({ queryKey: SEND_STATE_KEY });
}
