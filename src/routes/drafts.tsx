import { useMutation } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { Icon } from "../components/icon";
import { MailGate } from "../components/mail-provider";
import { useRefreshSend, useSendState } from "../components/send-state";
import { PageHeading } from "../components/workspace";
import { deleteDraft, errorMessage } from "../lib/native";

export const Route = createFileRoute("/drafts")({
  component: () => (
    <MailGate>
      <Drafts />
    </MailGate>
  ),
});

function Drafts() {
  const send = useSendState();
  const refresh = useRefreshSend();
  const remove = useMutation({ mutationFn: deleteDraft, onSuccess: refresh });
  const drafts = send.data?.drafts ?? [];
  return (
    <section className="page outgoing-page">
      <PageHeading
        title="Drafts"
        description="Saved as you type, encrypted on this device."
      >
        <Link to="/compose" search={{}} className="button button-primary">
          <Icon name="edit" size={14} />
          New message
        </Link>
      </PageHeading>
      {remove.isError && (
        <p role="alert" className="mail-inline-error">
          {errorMessage(remove.error)}
        </p>
      )}
      {drafts.length === 0 ? (
        <div className="outgoing-empty">
          <Icon name="file" size={24} />
          <p>No drafts. Anything you start writing is kept here.</p>
        </div>
      ) : (
        <ul className="outgoing-list" aria-label="Drafts">
          {drafts.map((draft) => (
            <li key={draft.id} className="outgoing-item">
              <Link
                to="/compose"
                search={{ draft: draft.id }}
                className="outgoing-item-main"
              >
                <strong>{draft.subject.trim() || "(no subject)"}</strong>
                <span>
                  {[draft.to, draft.cc].filter(Boolean).join(", ") ||
                    "No recipients yet"}
                </span>
                <time dateTime={new Date(draft.updated).toISOString()}>
                  Edited {new Date(draft.updated).toLocaleString()}
                </time>
              </Link>
              <button
                type="button"
                className="icon-button"
                aria-label={`Delete draft ${draft.subject || "(no subject)"}`}
                title="Delete draft"
                onClick={() => remove.mutate(draft.id)}
                disabled={remove.isPending}
              >
                <Icon name="trash" size={15} />
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
