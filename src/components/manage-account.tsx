import { useMutation } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import {
  errorMessage,
  removeMailAccount,
  updateMailPassword,
  type MailAccount,
} from "../lib/native";
import { useMail } from "./mail-provider";
import { Modal } from "./modal";

export function ManageAccount({
  account,
  onDismiss,
}: {
  account: MailAccount | null;
  onDismiss: () => void;
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
  const pending = update.isPending || remove.isPending;
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
          {confirmRemove ? (
            <>
              <p className="removal-note">
                This removes the account’s cached mail and saved credentials
                from this device. Mail on your server stays untouched.
              </p>
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
                  New password or app password
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
