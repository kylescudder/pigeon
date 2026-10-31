import { useMutation } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { errorMessage, returnToDrafts } from "../lib/native";
import { Icon } from "./icon";
import { useRefreshSend, useSendState } from "./send-state";

/** Shows each message still inside its undo delay, with a way to stop it. */
export function UndoSendBar() {
  const send = useSendState();
  const refresh = useRefreshSend();
  const navigate = useNavigate();
  const [now, setNow] = useState(() => Date.now());
  const waiting = (send.data?.outbox ?? []).filter(
    (item) =>
      item.send.state === "queued" &&
      !item.send.error &&
      item.send.send_at > now,
  );
  useEffect(() => {
    if (!waiting.length) return;
    const timer = window.setInterval(() => setNow(Date.now()), 250);
    return () => window.clearInterval(timer);
  }, [waiting.length]);
  // Keep the clock fresh when new items arrive from polling.
  useEffect(() => setNow(Date.now()), [send.data]);
  const undo = useMutation({
    mutationFn: returnToDrafts,
    onSuccess: async (draft) => {
      await refresh();
      await navigate({ to: "/compose", search: { draft: draft.id } });
    },
    onError: refresh,
  });
  if (!waiting.length && !undo.isError) return null;
  return (
    <div className="undo-send-stack" role="status" aria-live="polite">
      {waiting.map((item) => {
        const seconds = Math.max(
          1,
          Math.ceil(
            ((item.send.state === "queued" ? item.send.send_at : now) - now) /
              1000,
          ),
        );
        return (
          <div className="undo-send-bar" key={item.id}>
            <Icon name="send" size={15} />
            <span>
              Sending <strong>{item.subject || "(no subject)"}</strong> in{" "}
              {seconds}s
            </span>
            <button
              type="button"
              className="button button-secondary"
              onClick={() => undo.mutate(item.id)}
              disabled={undo.isPending}
            >
              Undo
            </button>
          </div>
        );
      })}
      {undo.isError && (
        <div className="undo-send-bar undo-send-error" role="alert">
          <Icon name="alert" size={15} />
          <span>{errorMessage(undo.error)}</span>
          <button
            type="button"
            className="button button-quiet"
            onClick={() => undo.reset()}
          >
            Dismiss
          </button>
        </div>
      )}
    </div>
  );
}
