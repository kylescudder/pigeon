import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { Icon } from "../components/icon";
import { MailGate, useMail } from "../components/mail-provider";
import { PageHeading } from "../components/workspace";
import {
  discardMailAction,
  errorMessage,
  mailActions,
  retryMailAction,
  type PendingMailAction,
} from "../lib/native";

export const Route = createFileRoute("/pending")({ component: PendingRoute });

function PendingRoute() {
  return (
    <MailGate>
      <Pending />
    </MailGate>
  );
}

const STATE_LABEL: Record<PendingMailAction["state"], string> = {
  queued: "Waiting",
  applying: "Applying",
  failed: "Failed",
  conflict: "Conflict",
};

function describe(action: PendingMailAction): string {
  const count = `${action.count} ${action.count === 1 ? "message" : "messages"}`;
  switch (action.kind) {
    case "mark_read":
      return `Mark ${count} read`;
    case "mark_unread":
      return `Mark ${count} unread`;
    case "archive":
      return `Archive ${count}`;
    case "trash":
      return `Move ${count} to Trash`;
    case "move":
      return `Move ${count} to ${action.target ?? "another folder"}`;
    case "delete_permanently":
      return `Permanently delete ${count}`;
  }
}

function Pending() {
  const mail = useMail();
  const actions = useQuery({
    queryKey: ["mail", "actions"],
    queryFn: mailActions,
    enabled: mail.desktop && !!mail.state,
    refetchInterval: 2000,
  });
  const retry = useMutation({
    mutationFn: retryMailAction,
    onSettled: mail.refresh,
  });
  const discard = useMutation({
    mutationFn: discardMailAction,
    onSettled: mail.refresh,
  });
  const items = actions.data ?? [];
  const accountName = (id: string) =>
    mail.state?.accounts.find((account) => account.id === id)?.config.name ??
    "Account";
  return (
    <section className="page pending-page">
      <PageHeading
        title="Pending changes"
        description="Changes are applied to the mail server in order. A change is complete only once the server has confirmed it."
      />
      {actions.isError ? (
        <p role="alert" className="mail-inline-error">
          {errorMessage(actions.error)}
        </p>
      ) : actions.isPending ? (
        <p>Loading pending changes…</p>
      ) : !items.length ? (
        <div className="list-empty">
          <Icon name="check" size={28} />
          <h2>Everything is up to date</h2>
          <p>No changes are waiting for the mail server.</p>
        </div>
      ) : (
        <ul className="pending-list" aria-label="Pending changes">
          {items.map((action) => {
            const attention =
              action.state === "failed" || action.state === "conflict";
            return (
              <li
                key={action.id}
                className={`pending-item${attention ? " attention" : ""}`}
              >
                <Icon
                  name={
                    attention
                      ? "alert"
                      : action.state === "applying"
                        ? "refresh"
                        : "info"
                  }
                  size={16}
                  className={action.state === "applying" ? "spinning" : undefined}
                />
                <div>
                  <strong>
                    {describe(action)}
                    <span className="pending-state">
                      {STATE_LABEL[action.state]}
                    </span>
                  </strong>
                  <p>
                    {accountName(action.account_id)} / {action.folder}
                    {action.subject ? ` · “${action.subject}”` : ""}
                  </p>
                  {action.error && (
                    <p className={attention ? "pending-error" : undefined}>
                      {action.state === "queued"
                        ? `Will retry when the account reconnects. Last attempt: ${action.error}`
                        : action.error}
                    </p>
                  )}
                  {action.state === "conflict" && (
                    <p>
                      Pigeon did not apply this change, so nothing was
                      re-created or moved elsewhere. Discard it to accept the
                      server’s version, or retry once you have checked.
                    </p>
                  )}
                </div>
                <div className="inline-actions">
                  {attention && (
                    <button
                      type="button"
                      className="button button-secondary"
                      disabled={retry.isPending}
                      onClick={() => retry.mutate(action.id)}
                    >
                      Retry
                    </button>
                  )}
                  <button
                    type="button"
                    className="button button-quiet"
                    disabled={discard.isPending || action.state === "applying"}
                    onClick={() => discard.mutate(action.id)}
                    title={
                      action.state === "queued"
                        ? "Cancel this change and restore the message locally"
                        : "Remove this change from the list"
                    }
                  >
                    Discard
                  </button>
                </div>
              </li>
            );
          })}
        </ul>
      )}
      {(retry.isError || discard.isError) && (
        <p role="alert" className="mail-inline-error">
          {errorMessage(retry.error ?? discard.error)}
        </p>
      )}
    </section>
  );
}
