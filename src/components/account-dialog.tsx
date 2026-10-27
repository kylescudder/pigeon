import { useMutation } from "@tanstack/react-query";
import { useEffect, useId, useState } from "react";
import { addMailAccount, errorMessage, type MailAccount } from "../lib/native";
import { Icon } from "./icon";
import { Modal } from "./modal";

export function AccountDialog({
  open,
  onDismiss,
  onConnected,
}: {
  open: boolean;
  onDismiss: () => void;
  onConnected: (account: MailAccount) => Promise<void>;
}) {
  const id = useId();
  const [name, setName] = useState("");
  const [address, setAddress] = useState("");
  const [host, setHost] = useState("");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [security, setSecurity] = useState<"tls" | "start_tls">("tls");
  const [port, setPort] = useState("993");
  const [certificate, setCertificate] = useState("");
  const connect = useMutation({
    mutationFn: () =>
      addMailAccount(
        {
          name: name.trim(),
          address: address.trim(),
          host: host.trim(),
          port: Number(port),
          username: username.trim() || address.trim(),
          security,
          ca_certificate: certificate.trim() || null,
        },
        password,
      ),
    onSuccess: async (account) => {
      await onConnected(account);
      onDismiss();
    },
    onSettled: () => setPassword(""),
  });
  useEffect(() => {
    if (!open) {
      setPassword("");
      connect.reset();
    }
  }, [open]);
  return (
    <Modal
      open={open}
      onDismiss={onDismiss}
      title="Connect an account"
      className="account-dialog"
      dismissible={!connect.isPending}
    >
      <div className="dialog-heading">
        <span className="surface-icon">
          <Icon name="mail" size={22} />
        </span>
        <h2>Connect an account</h2>
        <p>
          Your credentials stay in your desktop keyring. Pigeon connects
          directly to your mail server.
        </p>
      </div>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          connect.mutate();
        }}
      >
        <fieldset disabled={connect.isPending}>
          <div className="form-grid">
            <label htmlFor={`${id}-name`}>
              Account name
              <input
                id={`${id}-name`}
                value={name}
                onChange={(event) => setName(event.target.value)}
                placeholder="Personal"
                maxLength={120}
                required
              />
            </label>
            <label htmlFor={`${id}-address`}>
              Email address
              <input
                id={`${id}-address`}
                type="email"
                value={address}
                onChange={(event) => setAddress(event.target.value)}
                placeholder="you@example.com"
                required
              />
            </label>
          </div>
          <label htmlFor={`${id}-host`}>
            IMAP server
            <input
              id={`${id}-host`}
              value={host}
              onChange={(event) => setHost(event.target.value)}
              placeholder="imap.example.com"
              autoCapitalize="none"
              spellCheck={false}
              maxLength={253}
              required
            />
          </label>
          <label htmlFor={`${id}-username`}>
            Username
            <input
              id={`${id}-username`}
              value={username}
              onChange={(event) => setUsername(event.target.value)}
              placeholder={address || "Same as email address"}
              autoCapitalize="none"
              spellCheck={false}
            />
          </label>
          <label htmlFor={`${id}-password`}>
            Password or app password
            <input
              id={`${id}-password`}
              type="password"
              value={password}
              onChange={(event) => setPassword(event.target.value)}
              autoComplete="current-password"
              maxLength={8192}
              required
            />
          </label>
          <p className="field-help">
            Use an app password if your provider requires one. OAuth sign-in is
            not available in this build.
          </p>
          <details className="advanced-settings">
            <summary>Connection settings</summary>
            <div className="form-grid">
              <label htmlFor={`${id}-security`}>
                Encryption
                <select
                  id={`${id}-security`}
                  value={security}
                  onChange={(event) => {
                    const value = event.target.value as "tls" | "start_tls";
                    setSecurity(value);
                    setPort(value === "tls" ? "993" : "143");
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
                  required
                />
              </label>
            </div>
            <label htmlFor={`${id}-ca`}>
              Additional CA certificate
              <textarea
                id={`${id}-ca`}
                value={certificate}
                onChange={(event) => setCertificate(event.target.value)}
                placeholder="Optional PEM certificate for a private mail server"
                rows={4}
                spellCheck={false}
              />
            </label>
            <p className="field-help">
              Certificate and hostname verification always remain enabled.
            </p>
          </details>
        </fieldset>
        {connect.isError && (
          <div className="notice notice-error" role="alert">
            <Icon name="alert" size={17} />
            <p>{errorMessage(connect.error)}</p>
          </div>
        )}
        <div className="modal-actions">
          <button
            className="button button-quiet"
            type="button"
            onClick={onDismiss}
            disabled={connect.isPending}
          >
            Cancel
          </button>
          <button
            className="button button-primary"
            type="submit"
            disabled={connect.isPending}
          >
            {connect.isPending ? (
              <>
                <Icon name="refresh" size={14} className="spinning" />
                Connecting securely…
              </>
            ) : (
              "Connect account"
            )}
          </button>
        </div>
      </form>
    </Modal>
  );
}
