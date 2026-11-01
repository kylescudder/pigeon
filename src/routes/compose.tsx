import { savedSignatures } from "../lib/signatures";
import { useMutation } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { useEffect, useId, useRef, useState } from "react";
import { Icon } from "../components/icon";
import { MailGate, useMail } from "../components/mail-provider";
import { ManageAccount } from "../components/manage-account";
import { useRefreshSend, useSendState } from "../components/send-state";
import {
  deleteDraft,
  errorMessage,
  saveDraft,
  sendDraft,
  type Draft,
  type DraftInput,
  type MailAccount,
} from "../lib/native";

// The draft identifier is an opaque local id; addresses and text never go in
// the URL.
export const Route = createFileRoute("/compose")({
  validateSearch: (search: Record<string, unknown>): { draft?: string } => ({
    draft: typeof search.draft === "string" ? search.draft : undefined,
  }),
  component: () => (
    <MailGate>
      <ComposeLoader />
    </MailGate>
  ),
});

function ComposeLoader() {
  const { draft: draftId } = Route.useSearch();
  const send = useSendState();
  // Remount the composer whenever the URL's draft differs from the one it
  // shows: opening another draft, or a fresh message. Adding the id of a
  // draft it has just created to the URL must not remount it.
  const shown = useRef<string | null>(draftId ?? null);
  const key = useRef(draftId ?? `new-${Date.now()}`);
  if ((draftId ?? null) !== shown.current) {
    shown.current = draftId ?? null;
    key.current = draftId ?? `new-${Date.now()}`;
  }
  if (!send.data)
    return (
      <div className="page mail-empty">
        <Icon name="refresh" className="spinning" size={25} />
        <h1>Opening the composer…</h1>
      </div>
    );
  const draft = draftId
    ? send.data.drafts.find((candidate) => candidate.id === draftId)
    : undefined;
  return (
    <Composer
      key={key.current}
      initial={draft}
      onCreated={(id) => {
        shown.current = id;
      }}
    />
  );
}

const AUTOSAVE_MS = 800;

function Composer({
  initial,
  onCreated,
}: {
  initial: Draft | undefined;
  onCreated: (id: string) => void;
}) {
  const id = useId();
  const mail = useMail();
  const navigate = useNavigate();
  const refresh = useRefreshSend();
  const accounts = mail.state?.accounts ?? [];
  const defaultAccount =
    accounts.find((account) => account.id === initial?.account_id) ??
    accounts.find((account) => account.config.sending?.smtp) ??
    accounts[0];
  const [form, setForm] = useState<DraftInput>({
    id: initial?.id ?? null,
    account_id: defaultAccount?.id ?? null,
    to: initial?.to ?? "",
    cc: initial?.cc ?? "",
    bcc: initial?.bcc ?? "",
    subject: initial?.subject ?? "",
    body: initial?.body ?? "",
    signature: initial?.signature ?? null,
    // Kept as they are so a reply stays threaded through every autosave.
    in_reply_to: initial?.in_reply_to ?? "",
    references: initial?.references ?? "",
  });
  const [showCopies, setShowCopies] = useState(
    !!(initial?.cc || initial?.bcc),
  );
  const [dirty, setDirty] = useState(false);
  const [saveState, setSaveState] = useState<
    "idle" | "saving" | "saved" | { error: string }
  >(initial ? "saved" : "idle");
  const [managed, setManaged] = useState<MailAccount | null>(null);
  const formRef = useRef(form);
  formRef.current = form;
  const inFlight = useRef<Promise<unknown> | null>(null);
  const sending = useRef(false);

  // A new message gets its draft id from its first save.
  const adopt = (draftId: string) => {
    if (formRef.current.id) return;
    setForm((current) => ({ ...current, id: draftId }));
    formRef.current = { ...formRef.current, id: draftId };
    onCreated(draftId);
    void navigate({ to: "/compose", search: { draft: draftId }, replace: true });
  };

  // Saves happen one at a time so an older save can never land after a newer
  // one, or after the draft has been sent.
  const save = async () => {
    if (inFlight.current) await inFlight.current.catch(() => {});
    if (sending.current) return;
    setSaveState("saving");
    const pending = saveDraft(formRef.current).then(
      (saved) => {
        adopt(saved.id);
        setSaveState("saved");
        void refresh();
      },
      (error) => setSaveState({ error: errorMessage(error) }),
    );
    inFlight.current = pending;
    await pending;
    if (inFlight.current === pending) inFlight.current = null;
  };

  const unsaved = useRef(false);
  useEffect(() => {
    if (!dirty) return;
    unsaved.current = true;
    const timer = window.setTimeout(() => {
      unsaved.current = false;
      void save();
    }, AUTOSAVE_MS);
    return () => window.clearTimeout(timer);
  }, [form, dirty]);
  // Leaving the composer keeps the last keystrokes too.
  useEffect(
    () => () => {
      if (!unsaved.current || sending.current) return;
      // Wait for any first save so this reuses its draft id.
      void (inFlight.current ?? Promise.resolve())
        .catch(() => {})
        .then(() => saveDraft(formRef.current))
        .then(refresh, () => {});
    },
    [],
  );

  const submit = useMutation({
    mutationFn: async () => {
      sending.current = true;
      if (inFlight.current) await inFlight.current.catch(() => {});
      // Save first so a refused send leaves exactly one draft, with this id.
      if (!formRef.current.id) adopt((await saveDraft(formRef.current)).id);
      return sendDraft(formRef.current);
    },
    onSuccess: async () => {
      await refresh();
      await navigate({ to: "/outbox" });
    },
    onError: async () => {
      sending.current = false;
      await refresh();
    },
  });
  const discard = useMutation({
    mutationFn: async () => {
      sending.current = true;
      if (inFlight.current) await inFlight.current.catch(() => {});
      if (formRef.current.id) await deleteDraft(formRef.current.id);
    },
    onSuccess: async () => {
      await refresh();
      await navigate({ to: "/drafts" });
    },
    onError: () => {
      sending.current = false;
    },
  });

  const update = (patch: Partial<DraftInput>) => {
    setForm((current) => ({ ...current, ...patch }));
    setDirty(true);
    setSaveState("idle");
  };
  const account = accounts.find((candidate) => candidate.id === form.account_id);
  const signatures = savedSignatures(account?.config.sending);
  const signature = form.signature ?? account?.config.sending?.signature ?? "";
  const selectedSignature = signatures.find((item) => item.body === form.signature);
  const signatureChoice = form.signature == null ? "default"
    : form.signature === "" ? "none"
    : selectedSignature ? `saved:${selectedSignature.id}` : "custom";
  const canSend = !!account?.config.sending?.smtp;
  const busy = submit.isPending || discard.isPending;

  if (!accounts.length)
    return (
      <div className="page mail-empty">
        <Icon name="mail" size={32} />
        <h1>Connect an account to write mail</h1>
        <button
          type="button"
          className="button button-primary"
          onClick={mail.connect}
        >
          Connect an account
        </button>
      </div>
    );

  return (
    <section className="page compose-page">
      <form
        className="composer"
        aria-label="New message"
        onSubmit={(event) => {
          event.preventDefault();
          submit.mutate();
        }}
        onKeyDown={(event) => {
          if (
            event.key === "Enter" &&
            (event.metaKey || event.ctrlKey) &&
            !busy &&
            canSend
          ) {
            event.preventDefault();
            submit.mutate();
          }
        }}
      >
        <header className="composer-header">
          <h1>{form.subject.trim() || "New message"}</h1>
          <span className="composer-save-state" role="status">
            {saveState === "saving"
              ? "Saving…"
              : saveState === "saved"
                ? "Draft saved on this device"
                : saveState === "idle"
                  ? ""
                  : `Draft not saved: ${saveState.error}`}
          </span>
        </header>
        <fieldset disabled={busy} className="composer-fields">
          <div className="composer-row">
            <label htmlFor={`${id}-from`}>From</label>
            <select
              id={`${id}-from`}
              value={form.account_id ?? ""}
              onChange={(event) => update({ account_id: event.target.value, signature: null })}
            >
              {accounts.map((candidate) => (
                <option key={candidate.id} value={candidate.id}>
                  {candidate.config.sending?.display_name
                    ? `${candidate.config.sending.display_name} <${candidate.config.address}>`
                    : candidate.config.address}{" "}
                  · {candidate.config.name}
                </option>
              ))}
            </select>
          </div>
          <div className="composer-row">
            <label htmlFor={`${id}-to`}>To</label>
            <input
              id={`${id}-to`}
              value={form.to}
              onChange={(event) => update({ to: event.target.value })}
              autoCapitalize="none"
              spellCheck={false}
              autoFocus={!initial}
              placeholder="name@example.com, …"
            />
            {!showCopies && (
              <button
                type="button"
                className="composer-copies-toggle"
                onClick={() => setShowCopies(true)}
              >
                Cc/Bcc
              </button>
            )}
          </div>
          {showCopies && (
            <>
              <div className="composer-row">
                <label htmlFor={`${id}-cc`}>Cc</label>
                <input
                  id={`${id}-cc`}
                  value={form.cc}
                  onChange={(event) => update({ cc: event.target.value })}
                  autoCapitalize="none"
                  spellCheck={false}
                />
              </div>
              <div className="composer-row">
                <label htmlFor={`${id}-bcc`}>Bcc</label>
                <input
                  id={`${id}-bcc`}
                  value={form.bcc}
                  onChange={(event) => update({ bcc: event.target.value })}
                  autoCapitalize="none"
                  spellCheck={false}
                />
              </div>
            </>
          )}
          <div className="composer-row">
            <label htmlFor={`${id}-subject`}>Subject</label>
            <input
              id={`${id}-subject`}
              value={form.subject}
              onChange={(event) => update({ subject: event.target.value })}
              maxLength={998}
            />
          </div>
          <textarea
            className="composer-body"
            aria-label="Message"
            value={form.body}
            onChange={(event) => update({ body: event.target.value })}
            placeholder="Write your message"
          />
          <div className="composer-row">
            <label htmlFor={`${id}-signature`}>Signature</label>
            <select id={`${id}-signature`} value={signatureChoice} onChange={(event) => {
              const value = event.target.value;
              update({ signature: value === "default" ? null : value === "none" ? "" : signatures.find((item) => `saved:${item.id}` === value)?.body ?? "" });
            }}>
              <option value="default">Account default</option>
              <option value="none">No signature</option>
              {signatures.map((item) => <option key={item.id} value={`saved:${item.id}`}>{item.name}</option>)}
              {form.signature && !signatures.some((item) => item.body === form.signature) && <option value="custom">Saved with this draft</option>}
            </select>
          </div>
          {signature.trim() && (
            <pre className="composer-signature" aria-label="Signature preview">
              {`-- \n${signature}`}
            </pre>
          )}
        </fieldset>
        {account && !canSend && (
          <div className="notice" role="note">
            <Icon name="info" size={17} />
            <p>
              Add outgoing mail settings for {account.config.name} before
              sending. Your draft is kept.
            </p>
            <button
              type="button"
              className="button button-secondary"
              onClick={() => setManaged(account)}
            >
              Add outgoing mail
            </button>
          </div>
        )}
        {(submit.isError || discard.isError) && (
          <div className="notice notice-error" role="alert">
            <Icon name="alert" size={17} />
            <p>{errorMessage(submit.error ?? discard.error)}</p>
          </div>
        )}
        <footer className="composer-actions">
          <button
            type="button"
            className="button button-quiet"
            onClick={() => discard.mutate()}
            disabled={busy}
          >
            <Icon name="trash" size={14} />
            Discard
          </button>
          <Link to="/drafts" className="button button-quiet">
            Close
          </Link>
          <button
            type="submit"
            className="button button-primary"
            disabled={busy || !canSend}
            title="Send (Ctrl+Enter)"
          >
            <Icon name="send" size={14} />
            {submit.isPending ? "Queuing…" : "Send"}
          </button>
        </footer>
      </form>
      <ManageAccount
        account={managed}
        onDismiss={() => setManaged(null)}
        focusSending
      />
    </section>
  );
}
