import { useMutation } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { Icon } from "../components/icon";
import { MailGate, useMail } from "../components/mail-provider";
import { Modal } from "../components/modal";
import { useRefreshSend, useSendState } from "../components/send-state";
import { PageHeading } from "../components/workspace";
import {
  dismissOutbox,
  errorMessage,
  resolveSend,
  retrySend,
  retrySentCopy,
  returnToDrafts,
  type OutboxItem,
} from "../lib/native";

export const Route = createFileRoute("/outbox")({
  component: () => (
    <MailGate>
      <Outbox />
    </MailGate>
  ),
});

function Outbox() {
  const send = useSendState();
  const refresh = useRefreshSend();
  const navigate = useNavigate();
  const [confirm, setConfirm] = useState<OutboxItem | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const items = send.data?.outbox ?? [];
  const counting = items.some((item) => item.send.state === "queued");
  useEffect(() => {
    if (!counting) return;
    const timer = window.setInterval(() => setNow(Date.now()), 500);
    return () => window.clearInterval(timer);
  }, [counting]);

  const action = useMutation({
    mutationFn: async ({
      kind,
      id,
    }: {
      kind:
        | "edit"
        | "retry"
        | "retry_copy"
        | "dismiss"
        | "send_again"
        | "mark_sent"
        | "discard";
      id: string;
    }) => {
      switch (kind) {
        case "edit": {
          const draft = await returnToDrafts(id);
          // The composer reads the draft from send state when it mounts,
          // so that state must include it first.
          await refresh();
          await navigate({ to: "/compose", search: { draft: draft.id } });
          return;
        }
        case "retry":
          return retrySend(id);
        case "retry_copy":
          return retrySentCopy(id);
        case "dismiss":
          return dismissOutbox(id);
        default:
          return resolveSend(id, kind);
      }
    },
    onSettled: refresh,
  });
  const run = (kind: Parameters<typeof action.mutate>[0]["kind"], id: string) =>
    action.mutate({ kind, id });
  const busy = action.isPending;

  return (
    <section className="page outgoing-page">
      <PageHeading
        title="Outbox"
        description="Messages on their way, and what happened to them."
      />
      {action.isError && (
        <p role="alert" className="mail-inline-error">
          {errorMessage(action.error)}
        </p>
      )}
      {items.length === 0 ? (
        <div className="outgoing-empty">
          <Icon name="outbox" size={24} />
          <p>Nothing waiting to send.</p>
        </div>
      ) : (
        <ul className="outgoing-list" aria-label="Outbox">
          {items.map((item) => (
            <li
              key={item.id}
              className={`outgoing-item outbox-${item.send.state}`}
            >
              <div className="outgoing-item-main">
                <strong>{item.subject || "(no subject)"}</strong>
                <span>To {item.recipients}</span>
                <AccountName id={item.account_id} />
                <Status item={item} now={now} />
              </div>
              <div className="outgoing-actions">
                {item.send.state === "queued" && (
                  <>
                    <button
                      type="button"
                      className="button button-secondary"
                      disabled={busy}
                      onClick={() => run("retry", item.id)}
                    >
                      {item.send.error ? "Retry now" : "Send now"}
                    </button>
                    <button
                      type="button"
                      className="button button-quiet"
                      disabled={busy}
                      onClick={() => run("edit", item.id)}
                    >
                      {item.send.error ? "Edit" : "Undo"}
                    </button>
                  </>
                )}
                {item.send.state === "failed" && (
                  <>
                    <button
                      type="button"
                      className="button button-secondary"
                      disabled={busy}
                      onClick={() => run("retry", item.id)}
                    >
                      Retry
                    </button>
                    <button
                      type="button"
                      className="button button-quiet"
                      disabled={busy}
                      onClick={() => run("edit", item.id)}
                    >
                      Edit
                    </button>
                    <button
                      type="button"
                      className="button button-quiet"
                      disabled={busy}
                      onClick={() => run("dismiss", item.id)}
                    >
                      Discard
                    </button>
                  </>
                )}
                {(item.send.state === "unknown" || item.send.state === "held") && (
                  <>
                    <button
                      type="button"
                      className="button button-secondary"
                      disabled={busy}
                      onClick={() => run("mark_sent", item.id)}
                    >
                      It was sent
                    </button>
                    <button
                      type="button"
                      className="button button-quiet"
                      disabled={busy}
                      onClick={() => setConfirm(item)}
                    >
                      Send again…
                    </button>
                    <button
                      type="button"
                      className="button button-quiet"
                      disabled={busy}
                      onClick={() => run("discard", item.id)}
                    >
                      Discard
                    </button>
                  </>
                )}
                {item.send.state === "accepted" && (
                  <>
                    {item.sent_copy.state === "failed" && (
                      <button
                        type="button"
                        className="button button-secondary"
                        disabled={busy}
                        onClick={() => run("retry_copy", item.id)}
                      >
                        Save to Sent
                      </button>
                    )}
                    <button
                      type="button"
                      className="button button-quiet"
                      disabled={busy}
                      onClick={() => run("dismiss", item.id)}
                    >
                      Clear
                    </button>
                  </>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
      <Modal
        open={!!confirm}
        onDismiss={() => setConfirm(null)}
        title="Send again"
        className="form-dialog"
      >
        <h2>Send this message again?</h2>
        <p>
          The server may already have accepted it, so recipients could get it
          twice. Check your Sent folder or ask a recipient first if you can.
        </p>
        <div className="modal-actions">
          <button
            type="button"
            className="button button-quiet"
            onClick={() => setConfirm(null)}
          >
            Cancel
          </button>
          <button
            type="button"
            className="button button-primary"
            onClick={() => {
              if (confirm) run("send_again", confirm.id);
              setConfirm(null);
            }}
          >
            Send again
          </button>
        </div>
      </Modal>
    </section>
  );
}

function AccountName({ id }: { id: string }) {
  const mail = useMail();
  const account = mail.state?.accounts.find((candidate) => candidate.id === id);
  return account ? <span>From {account.config.name}</span> : null;
}

function Status({ item, now }: { item: OutboxItem; now: number }) {
  const send = item.send;
  const copy = item.sent_copy;
  switch (send.state) {
    case "queued": {
      if (send.error)
        return (
          <p className="outgoing-status status-waiting">
            <Icon name="refresh" size={13} />
            Waiting to send. Last attempt: {send.error}
          </p>
        );
      const seconds = Math.ceil((send.send_at - now) / 1000);
      return (
        <p className="outgoing-status status-waiting">
          <Icon name="send" size={13} />
          {seconds > 0 ? `Sending in ${seconds}s` : "Sending shortly"}
        </p>
      );
    }
    case "sending":
      return (
        <p className="outgoing-status status-waiting">
          <Icon name="refresh" size={13} className="spinning" />
          Sending…
        </p>
      );
    case "held":
      return (
        <p className="outgoing-status status-waiting">
          <Icon name="lock" size={13} />
          Paused until you review it
        </p>
      );
    case "failed":
      return (
        <p className="outgoing-status status-failed">
          <Icon name="alert" size={13} />
          Not sent: {send.error}
        </p>
      );
    case "unknown":
      return (
        <div className="outgoing-status status-unknown">
          <p>
            <Icon name="alert" size={13} />
            <strong>Pigeon can’t tell whether this was sent.</strong>{" "}
            {send.error}
          </p>
          <p>
            It won’t be sent again unless you choose to. If it reached the
            server, it may already be in your Sent folder or with recipients.
          </p>
        </div>
      );
    case "accepted":
      return (
        <p className="outgoing-status status-sent">
          <Icon name="checkCircle" size={13} />
          Sent {new Date(send.at).toLocaleTimeString()}
          {copy.state === "saving" && " · Saving to Sent…"}
          {copy.state === "waiting" &&
            (copy.error
              ? ` · Not yet saved to Sent, retrying: ${copy.error}`
              : " · Saving to Sent…")}
          {copy.state === "failed" && ` · Not saved to Sent: ${copy.error}`}
        </p>
      );
  }
}
