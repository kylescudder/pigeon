import { Link } from "@tanstack/react-router";
import { useMutation } from "@tanstack/react-query";
import { useEffect, useId, useState } from "react";
import {
  errorMessage,
  removeMailAccount,
  updateMailPassword,
  updateMailSending,
  type MailAccount,
  type TransportSecurity,
} from "../lib/native";
import { useMail } from "./mail-provider";
import { Modal } from "./modal";

export function ManageAccount({
  account,
  onDismiss,
  focusSending = false,
}: {
  account: MailAccount | null;
  onDismiss: () => void;
  /** Open with the outgoing mail settings expanded. */
  focusSending?: boolean;
}) {
  const mail = useMail();
  const [password, setPassword] = useState("");
  const [confirmRemove, setConfirmRemove] = useState(false);
  const update = useMutation({
    mutationFn: () => updateMailPassword(account!.id, password),
    onSuccess: async () => {
      await mail.refresh();
    },
    onSettled: () => setPassword(""),
  });
  const remove = useMutation({
    mutationFn: () => removeMailAccount(account!.id),
    onSuccess: async () => {
      await mail.refresh();
      onDismiss();
    },
  });
  useEffect(() => {
    setPassword("");
    setConfirmRemove(false);
    update.reset();
    remove.reset();
  }, [account?.id]);
  const managed = !!account?.config.managed;
  const pending = update.isPending || remove.isPending;
  // The mail state is recounted every few seconds, so this falls as removal
  // works through the account's batches.
  const remaining = (mail.state?.folders ?? [])
    .filter((folder) => folder.account_id === account?.id)
    .reduce((sum, folder) => sum + folder.cached, 0);
  return (
    <Modal
      open={!!account}
      onDismiss={onDismiss}
      title={confirmRemove ? "Remove account" : "Manage account"}
      className="form-dialog"
      dismissible={!pending}
    >
      {account && (
        <>
          <h2>
            {confirmRemove ? "Remove this account?" : account.config.name}
          </h2>
          <p>{account.config.address}</p>
          {managed && (
            <p className="field-help">
              Managed by Home Manager. Server and sending settings come from{" "}
              <code>accounts.email.accounts</code>; change them in your Nix
              configuration. Your password stays on this device.
            </p>
          )}
          {confirmRemove ? (
            <>
              <p className="removal-note">
                This removes the account’s cached mail and saved credentials
                from this device. Mail on your server stays untouched.
              </p>
              {remove.isPending && (
                <p role="status" className="removal-note">
                  {remaining > 0
                    ? `${remaining.toLocaleString()} cached messages left to remove.`
                    : "Removing saved credentials…"}
                </p>
              )}
              {remove.isError && (
                <p role="alert" className="mail-inline-error">
                  {errorMessage(remove.error)}
                </p>
              )}
              <div className="modal-actions">
                <button
                  type="button"
                  className="button button-quiet"
                  onClick={() => setConfirmRemove(false)}
                  disabled={pending}
                >
                  Keep account
                </button>
                <button
                  type="button"
                  className="button button-danger"
                  onClick={() => remove.mutate()}
                  disabled={pending}
                >
                  {remove.isPending ? "Removing…" : "Remove account"}
                </button>
              </div>
            </>
          ) : (
            <>
              <dl className="account-connection-details">
                <div>
                  <dt>Server</dt>
                  <dd>
                    {account.config.host}:{account.config.port}
                  </dd>
                </div>
                <div>
                  <dt>Username</dt>
                  <dd>{account.config.username}</dd>
                </div>
                <div>
                  <dt>Connection</dt>
                  <dd>
                    {account.config.security === "tls" ? "TLS" : "STARTTLS"}
                  </dd>
                </div>
              </dl>
              <form
                onSubmit={(event) => {
                  event.preventDefault();
                  update.mutate();
                }}
              >
                <label htmlFor="replacement-password">
                  {account.error && managed
                    ? "Password or app password"
                    : "New password or app password"}
                  <input
                    id="replacement-password"
                    type="password"
                    value={password}
                    onChange={(event) => setPassword(event.target.value)}
                    required
                    maxLength={8192}
                    disabled={pending}
                  />
                </label>
                {update.isError && (
                  <p role="alert" className="mail-inline-error">
                    {errorMessage(update.error)}
                  </p>
                )}
                {update.isSuccess && (
                  <p role="status" className="inline-success">
                    Password verified and updated.
                  </p>
                )}
                <div className="modal-actions">
                  <button
                    type="submit"
                    className="button button-secondary"
                    disabled={pending}
                  >
                    {update.isPending ? "Verifying…" : "Update password"}
                  </button>
                </div>
              </form>
              {managed ? (
                <dl className="account-connection-details">
                  <div>
                    <dt>Outgoing server</dt>
                    <dd>
                      {account.config.sending?.smtp
                        ? `${account.config.sending.smtp.host}:${account.config.sending.smtp.port}`
                        : "Not set up"}
                    </dd>
                  </div>
                </dl>
              ) : (
                <SendingForm
                  key={account.id}
                  account={account}
                  open={focusSending || !account.config.sending?.smtp}
                  disabled={pending}
                  onOpenSignatures={onDismiss}
                />
              )}
              {!managed && (
                <div className="account-danger-zone">
                  <p>Remove this account’s data from this device.</p>
                  <button
                    type="button"
                    className="button button-danger"
                    onClick={() => setConfirmRemove(true)}
                    disabled={
                      pending ||
                      mail.active.has(account.id) ||
                      account.status === "syncing"
                    }
                  >
                    Remove from this device
                  </button>
                </div>
              )}
              <div className="modal-actions">
                <button
                  type="button"
                  className="button button-primary"
                  onClick={onDismiss}
                  disabled={pending}
                >
                  Done
                </button>
              </div>
            </>
          )}
        </>
      )}
    </Modal>
  );
}

/** Outgoing server, display name and Sent-copy preference. */
function SendingForm({
  account,
  open,
  disabled,
  onOpenSignatures,
}: {
  account: MailAccount;
  open: boolean;
  disabled: boolean;
  onOpenSignatures: () => void;
}) {
  const id = useId();
  const mail = useMail();
  const current = account.config.sending;
  const [host, setHost] = useState(
    current?.smtp?.host ?? account.config.host.replace(/^imap\./i, "smtp."),
  );
  const [security, setSecurity] = useState<TransportSecurity>(
    current?.smtp?.security ?? "tls",
  );
  const [port, setPort] = useState(String(current?.smtp?.port ?? 465));
  const [username, setUsername] = useState(
    current?.smtp?.username ?? account.config.username,
  );
  const [displayName, setDisplayName] = useState(current?.display_name ?? "");
  const [saveSentCopy, setSaveSentCopy] = useState(
    current?.save_sent_copy ?? true,
  );
  const save = useMutation({
    mutationFn: () =>
      updateMailSending(account.id, {
        ...current,
        smtp: host.trim()
          ? {
              host: host.trim(),
              port: Number(port),
              security,
              username: username.trim() || account.config.username,
            }
          : null,
        display_name: displayName.trim(),
        signature: current?.signature ?? "",
        save_sent_copy: saveSentCopy,
      }),
    onSuccess: mail.refresh,
  });
  return (
    <details className="advanced-settings sending-settings" open={open}>
      <summary>Sending</summary>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <fieldset disabled={disabled || save.isPending}>
          <label htmlFor={`${id}-name`}>
            Name shown to recipients
            <input
              id={`${id}-name`}
              value={displayName}
              onChange={(event) => setDisplayName(event.target.value)}
              placeholder={account.config.address}
              maxLength={200}
            />
          </label>
          <p className="field-help">
            Edit your signature in{" "}
            <Link to="/settings/signatures" onClick={onOpenSignatures}>
              Signatures settings
            </Link>.
          </p>
          <label htmlFor={`${id}-host`}>
            Outgoing (SMTP) server
            <input
              id={`${id}-host`}
              value={host}
              onChange={(event) => setHost(event.target.value)}
              placeholder="smtp.example.com"
              autoCapitalize="none"
              spellCheck={false}
              maxLength={253}
            />
          </label>
          <div className="form-grid">
            <label htmlFor={`${id}-security`}>
              Encryption
              <select
                id={`${id}-security`}
                value={security}
                onChange={(event) => {
                  const value = event.target.value as TransportSecurity;
                  setSecurity(value);
                  setPort(value === "tls" ? "465" : "587");
                }}
              >
                <option value="tls">TLS</option>
                <option value="start_tls">STARTTLS</option>
              </select>
            </label>
            <label htmlFor={`${id}-port`}>
              Port
              <input
                id={`${id}-port`}
                type="number"
                value={port}
                onChange={(event) => setPort(event.target.value)}
                min={1}
                max={65535}
              />
            </label>
          </div>
          <label htmlFor={`${id}-username`}>
            Outgoing username
            <input
              id={`${id}-username`}
              value={username}
              onChange={(event) => setUsername(event.target.value)}
              autoCapitalize="none"
              spellCheck={false}
            />
          </label>
          <label className="checkbox-row" htmlFor={`${id}-copy`}>
            <input
              id={`${id}-copy`}
              type="checkbox"
              checked={saveSentCopy}
              onChange={(event) => setSaveSentCopy(event.target.checked)}
            />
            Save a copy of sent mail to the Sent folder
          </label>
          <p className="field-help">
            Pigeon signs in with this account’s saved password. Turn off the
            Sent copy if your provider files sent mail itself.
          </p>
        </fieldset>
        {save.isError && (
          <p role="alert" className="mail-inline-error">
            {errorMessage(save.error)}
          </p>
        )}
        {save.isSuccess && (
          <p role="status" className="inline-success">
            Sending settings saved.
          </p>
        )}
        <div className="modal-actions">
          <button
            type="submit"
            className="button button-secondary"
            disabled={disabled || save.isPending}
          >
            {save.isPending ? "Checking the server…" : "Save sending settings"}
          </button>
        </div>
      </form>
    </details>
  );
}
