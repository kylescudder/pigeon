import { useMutation } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useId, useState } from "react";
import { MailGate, useMail } from "../components/mail-provider";
import { PageHeading } from "../components/workspace";
import { errorMessage, updateMailSending, type MailAccount, type SavedSignature } from "../lib/native";

import { savedSignatures } from "../lib/signatures";

export const Route = createFileRoute("/settings_/signatures")({
  component: () => (
    <MailGate>
      <Signatures />
    </MailGate>
  ),
});

function Signatures() {
  const mail = useMail();
  const accounts = mail.state?.accounts ?? [];
  return (
    <section className="page settings-page">
      <PageHeading
        title="Signatures"
        description="Save signatures for each account, choose a default, and switch when writing a message."
      >
        <Link to="/settings" className="button button-secondary">
          Back to settings
        </Link>
      </PageHeading>
      {accounts.length === 0 && <p>Connect an account to add a signature.</p>}
      {accounts.map((account) => (
        <SignatureForm key={account.id} account={account} />
      ))}
    </section>
  );
}

function SignatureForm({ account }: { account: MailAccount }) {
  const id = useId();
  const mail = useMail();
  const [signatures, setSignatures] = useState<SavedSignature[]>(() => {
    const saved = savedSignatures(account.config.sending);
    return saved.length || account.config.sending?.signatures
      ? saved
      : [{ id: "default", name: "Default", body: "" }];
  });
  const [selectedId, setSelectedId] = useState(signatures[0]?.id ?? "");
  const [defaultId, setDefaultId] = useState(
    signatures.find((item) => item.body === account.config.sending?.signature)?.id ?? "",
  );
  const selected = signatures.find((item) => item.id === selectedId);
  const managed = !!account.config.managed;
  const save = useMutation({
    mutationFn: () => updateMailSending(account.id, {
      smtp: null,
      display_name: "",
      save_sent_copy: true,
      ...account.config.sending,
      signatures,
      signature: signatures.find((item) => item.id === defaultId)?.body ?? "",
    }),
    onSuccess: mail.refresh,
  });
  const locked = managed || save.isPending;
  function edit(patch: Partial<SavedSignature>) {
    setSignatures((items) => items.map((item) => item.id === selectedId ? { ...item, ...patch } : item));
    save.reset();
  }
  return (
    <form
      className="settings-section signature-settings"
      aria-labelledby={`${id}-heading`}
      onSubmit={(event) => {
        event.preventDefault();
        save.mutate();
      }}
    >
      <h2 id={`${id}-heading`}>{account.config.name}</h2>
      <p>{account.config.address}</p>
      <fieldset disabled={locked}>
        <label htmlFor={`${id}-default`}>Default signature</label>
        <select id={`${id}-default`} value={defaultId} onChange={(event) => {
          setDefaultId(event.target.value);
          save.reset();
        }}>
          <option value="">No signature</option>
          {signatures.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}
        </select>
        {signatures.length > 0 && <>
          <label htmlFor={`${id}-saved`}>Saved signature</label>
          <select id={`${id}-saved`} value={selectedId} onChange={(event) => setSelectedId(event.target.value)}>
            {signatures.map((item) => <option key={item.id} value={item.id}>{item.name}</option>)}
          </select>
        </>}
        {selected && <>
          <label htmlFor={`${id}-name`}>Signature name</label>
          <input id={`${id}-name`} required maxLength={100} value={selected.name} onChange={(event) => edit({ name: event.target.value })} />
          <label htmlFor={`${id}-signature`}>Signature</label>
          <textarea id={`${id}-signature`} rows={5} maxLength={10000} value={selected.body} onChange={(event) => edit({ body: event.target.value })} />
        </>}
        {!managed && <div className="inline-actions signature-tools">
          <button type="button" className="button button-secondary" disabled={signatures.length >= 20} onClick={() => {
            const item = { id: crypto.randomUUID(), name: `Signature ${signatures.length + 1}`, body: "" };
            setSignatures((items) => [...items, item]);
            setSelectedId(item.id);
            save.reset();
          }}>Add signature</button>
          {selected && <button type="button" className="button button-quiet" onClick={() => {
            const remaining = signatures.filter((item) => item.id !== selectedId);
            setSignatures(remaining);
            setSelectedId(remaining[0]?.id ?? "");
            if (defaultId === selectedId) setDefaultId("");
            save.reset();
          }}>Remove signature</button>}
        </div>}
      </fieldset>
      {managed && <p>Managed by Home Manager. Change the signature in your Nix configuration.</p>}
      {save.isError && <p role="alert" className="mail-inline-error">Could not save signature: {errorMessage(save.error)}</p>}
      {save.isSuccess && <p role="status" className="inline-success">Signature saved.</p>}
      {!managed && <div className="modal-actions">
        <button type="submit" className="button button-primary" disabled={save.isPending}>
          {save.isPending ? "Saving…" : "Save signature"}
        </button>
      </div>}
    </form>
  );
}
