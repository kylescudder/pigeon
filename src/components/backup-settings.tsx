import { useMutation } from "@tanstack/react-query";
import { useState, type FormEvent } from "react";
import {
  backupMail,
  cancelMailSync,
  errorMessage,
  restoreMail,
  type BackupSummary,
} from "../lib/native";
import { Icon } from "./icon";
import { useMail } from "./mail-provider";
import { Modal } from "./modal";

export const MIN_PASSPHRASE = 12;

function counts(summary: BackupSummary) {
  const plural = (n: number, one: string, many: string) =>
    `${n.toLocaleString()} ${n === 1 ? one : many}`;
  return [
    plural(summary.accounts, "account", "accounts"),
    plural(summary.passwords, "password", "passwords"),
    plural(summary.saved_searches, "saved search", "saved searches"),
  ].join(", ");
}

export function BackupSettings() {
  const mail = useMail();
  const [dialog, setDialog] = useState<"backup" | "restore" | null>(null);
  const [result, setResult] = useState<string | null>(null);
  return (
    <section className="settings-section backup-section" id="backup">
      <h2>Backup and restore</h2>
      <p>
        Back up what exists only on this device: account settings and
        passwords, saved searches, drafts, unsent mail and pending mailbox changes. Mail
        on your servers is not included. Pigeon downloads it again after a
        restore.
      </p>
      <ul className="backup-facts" aria-label="What you can lose">
        <li>
          <Icon name="lock" size={14} />
          <span>
            Backups are encrypted with a passphrase you choose. Pigeon does not
            keep it, and nobody can open a backup without it.
          </span>
        </li>
        <li>
          <Icon name="shield" size={14} />
          <span>
            If this device’s keyring loses Pigeon’s key and you have no backup
            or have forgotten its passphrase, local-only data is gone for good.
            Mail on your servers is unaffected.
          </span>
        </li>
        <li>
          <Icon name="archive" size={14} />
          <span>
            A backup contains your account passwords, so look after it as
            carefully as your keyring. Full-disk encryption still protects
            everything else on this device. Neither protects against malware or
            anyone using your unlocked session.
          </span>
        </li>
        <li>
          <Icon name="check" size={14} />
          <span>
            Restoring never sends mail or applies mailbox changes. Outgoing work stays paused
            until you review it.
          </span>
        </li>
      </ul>
      {mail.desktop ? (
        <div className="inline-actions backup-actions">
          <button
            type="button"
            className="button button-primary"
            onClick={() => {
              setResult(null);
              setDialog("backup");
            }}
          >
            Back up now…
          </button>
          <button
            type="button"
            className="button button-secondary"
            onClick={() => {
              setResult(null);
              setDialog("restore");
            }}
          >
            Restore from backup…
          </button>
        </div>
      ) : (
        <p className="backup-unavailable">
          Backup and restore are available in Pigeon’s desktop app.
        </p>
      )}
      {result && (
        <p role="status" className="inline-success backup-result">
          {result}
        </p>
      )}
      <PassphraseDialog
        mode={dialog}
        onDismiss={() => setDialog(null)}
        onDone={(message) => {
          setDialog(null);
          setResult(message);
        }}
      />
    </section>
  );
}

function PassphraseDialog({
  mode,
  onDismiss,
  onDone,
}: {
  mode: "backup" | "restore" | null;
  onDismiss: () => void;
  onDone: (message: string) => void;
}) {
  const mail = useMail();
  const [passphrase, setPassphrase] = useState("");
  const [confirm, setConfirm] = useState("");
  const [problem, setProblem] = useState<string | null>(null);
  const reset = () => {
    setPassphrase("");
    setConfirm("");
    setProblem(null);
    run.reset();
  };
  const run = useMutation({
    mutationFn: async () => {
      if (mode === "backup") return backupMail(passphrase);
      // A sync in progress would refuse the restore; stop them first.
      await Promise.all(
        (mail.state?.accounts ?? []).map((account) =>
          cancelMailSync(account.id).catch(() => {}),
        ),
      );
      return restoreMail(passphrase);
    },
    onSuccess: async (summary) => {
      // Null means the file chooser was cancelled; keep the dialog open.
      if (!summary) return;
      if (mode === "restore") await mail.refresh();
      reset();
      onDone(
        mode === "backup"
          ? `Backup saved with ${counts(summary)}.`
          : `Restored ${counts(summary)}. Pigeon is downloading your mail again.` +
              (summary.previous_store
                ? ` The replaced store was kept as ${summary.previous_store} in Pigeon’s data folder.`
                : ""),
      );
    },
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (mode === "backup") {
      if ([...passphrase].length < MIN_PASSPHRASE)
        return setProblem(
          `Use a passphrase of at least ${MIN_PASSPHRASE} characters.`,
        );
      if (passphrase !== confirm)
        return setProblem("The passphrases do not match.");
    }
    setProblem(null);
    run.mutate();
  };
  const pending = run.isPending;
  const backup = mode === "backup";
  return (
    <Modal
      open={mode !== null}
      onDismiss={() => {
        reset();
        onDismiss();
      }}
      title={backup ? "Back up local data" : "Restore from backup"}
      className="form-dialog"
      dismissible={!pending}
    >
      <h2>{backup ? "Back up local data" : "Restore from a backup"}</h2>
      <p>
        {backup
          ? "Choose a passphrase you will remember. Without it the backup cannot be opened, and Pigeon cannot reset it."
          : "Restoring replaces local accounts, saved searches, drafts and queued work on this device with the backup’s. The current store is kept aside, not deleted. Syncing stops while the restore runs."}
      </p>
      <form onSubmit={submit} noValidate>
        <label htmlFor="backup-passphrase">
          {backup ? "Backup passphrase" : "The backup’s passphrase"}
          <input
            id="backup-passphrase"
            type="password"
            autoComplete={backup ? "new-password" : "current-password"}
            value={passphrase}
            onChange={(event) => setPassphrase(event.target.value)}
            maxLength={1024}
            disabled={pending}
            autoFocus
          />
        </label>
        {backup && (
          <label htmlFor="backup-passphrase-confirm">
            Type it again
            <input
              id="backup-passphrase-confirm"
              type="password"
              autoComplete="new-password"
              value={confirm}
              onChange={(event) => setConfirm(event.target.value)}
              maxLength={1024}
              disabled={pending}
            />
          </label>
        )}
        {(problem || run.isError) && (
          <p role="alert" className="mail-inline-error">
            {problem ?? errorMessage(run.error)}
          </p>
        )}
        <div className="modal-actions">
          <button
            type="button"
            className="button button-quiet"
            onClick={() => {
              reset();
              onDismiss();
            }}
            disabled={pending}
          >
            Cancel
          </button>
          <button
            type="submit"
            className={`button ${backup ? "button-primary" : "button-danger"}`}
            disabled={pending || passphrase.length === 0}
          >
            {pending
              ? backup
                ? "Backing up…"
                : "Restoring…"
              : backup
                ? "Choose where to save…"
                : "Choose backup file…"}
          </button>
        </div>
      </form>
    </Modal>
  );
}
