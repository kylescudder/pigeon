import { useMutation } from "@tanstack/react-query";
import { useId, useState } from "react";
import { errorMessage, updateMailPassword, type MailAccount } from "../lib/native";
import { useMail } from "./mail-provider";
import { Modal } from "./modal";

export function AccountPasswordPrompt() {
  const mail = useMail();
  // A skipped account can still be managed manually; ask again next launch.
  const [handled, setHandled] = useState(new Set<string>());
  const account = mail.state?.accounts.find(
    (account) =>
      // This is the engine's missing-password error, also used for newly
      // declared accounts before their first sync. Other errors need no prompt.
      account.error === "enter this account’s password to start syncing" &&
      !handled.has(account.id),
  );
  if (!account) return null;
  return (
    <PasswordDialog
      key={account.id}
      account={account}
      onDismiss={() => setHandled((previous) => new Set(previous).add(account.id))}
    />
  );
}

function PasswordDialog({
  account,
  onDismiss,
}: {
  account: MailAccount;
  onDismiss: () => void;
}) {
  const mail = useMail();
  const id = useId();
  const [password, setPassword] = useState("");
  const save = useMutation({
    mutationFn: () => updateMailPassword(account.id, password),
    onSuccess: () => {
      onDismiss();
      void mail.sync(account.id);
    },
    onSettled: () => setPassword(""),
  });
  return (
    <Modal
      open
      onDismiss={onDismiss}
      title="Enter account password"
      className="account-dialog"
      dismissible={!save.isPending}
    >
      <div className="dialog-heading">
        <h2>Enter account password</h2>
        <p>
          {account.config.name} · {account.config.address}
        </p>
        <p>
          Enter your password to start syncing this account. It stays in your
          desktop keyring.
        </p>
      </div>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          save.mutate();
        }}
      >
        <label htmlFor={id}>
          Password or app password
          <input
            id={id}
            type="password"
            autoComplete="current-password"
            autoFocus
            value={password}
            onChange={(event) => setPassword(event.target.value)}
            required
            maxLength={8192}
            disabled={save.isPending}
          />
        </label>
        {save.isError && (
          <p role="alert" className="mail-inline-error">
            {errorMessage(save.error)}
          </p>
        )}
        <div className="modal-actions">
          <button
            type="button"
            className="button button-quiet"
            onClick={onDismiss}
            disabled={save.isPending}
          >
            Not now
          </button>
          <button
            type="submit"
            className="button button-primary"
            disabled={save.isPending}
          >
            {save.isPending ? "Verifying…" : "Save password"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
