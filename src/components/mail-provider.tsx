import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import {
  createContext,
  useContext,
  useEffect,
  useState,
  type ReactNode,
} from "react";
import {
  cancelMailSync,
  errorCode,
  errorMessage,
  isDesktop,
  mailState,
  syncMailAccount,
  unlockMail,
  type MailState,
} from "../lib/native";
import { AccountDialog } from "./account-dialog";
import { Icon } from "./icon";

type MailContextValue = {
  state: MailState | undefined;
  desktop: boolean;
  loading: boolean;
  error: unknown;
  actionError: string | null;
  clearActionError: () => void;
  connect: () => void;
  refresh: () => Promise<void>;
  sync: (id: string) => Promise<void>;
  cancelSync: (id: string) => Promise<void>;
  active: Set<string>;
};
const MailContext = createContext<MailContextValue | null>(null);
export const MAIL_STATE_KEY = ["mail", "state"] as const;

export function useMail() {
  const context = useContext(MailContext);
  if (!context) throw new Error("Mail provider is missing");
  return context;
}

export function MailProvider({ children }: { children: ReactNode }) {
  const client = useQueryClient();
  const [desktop, setDesktop] = useState(false);
  const [ready, setReady] = useState(false);
  const [connectOpen, setConnectOpen] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);
  const [active, setActive] = useState(new Set<string>());
  useEffect(() => {
    setDesktop(isDesktop());
    setReady(true);
  }, []);
  const stateQuery = useQuery({
    queryKey: MAIL_STATE_KEY,
    queryFn: mailState,
    enabled: desktop,
    refetchInterval: (query) =>
      active.size > 0 ||
      query.state.data?.accounts.some((account) => account.status === "syncing")
        ? 1000
        : 5000,
  });
  useEffect(() => {
    if (stateQuery.isError) {
      // Unmount private views and discard cached bodies when the encrypted
      // store becomes unavailable. Never keep showing a previous success.
      client.removeQueries({ queryKey: ["mail", "search"] });
      client.removeQueries({ queryKey: ["mail", "message"] });
    }
  }, [stateQuery.isError, client]);
  const refresh = async () => {
    await client.invalidateQueries({ queryKey: MAIL_STATE_KEY });
    await client.invalidateQueries({ queryKey: ["mail", "search"] });
    await client.invalidateQueries({ queryKey: ["mail", "message"] });
  };
  const sync = async (id: string) => {
    setActionError(null);
    setActive((previous) => new Set(previous).add(id));
    try {
      await syncMailAccount(id);
    } catch (error) {
      if (!["busy", "cancelled"].includes(errorCode(error) ?? ""))
        setActionError(errorMessage(error));
    } finally {
      setActive((previous) => {
        const next = new Set(previous);
        next.delete(id);
        return next;
      });
      await refresh();
    }
  };
  const cancelSync = async (id: string) => {
    try {
      await cancelMailSync(id);
      await refresh();
    } catch (error) {
      setActionError(errorMessage(error));
    }
  };
  const state = stateQuery.isError ? undefined : stateQuery.data;
  return (
    <MailContext.Provider
      value={{
        state,
        desktop,
        loading: !ready || (desktop && stateQuery.isPending),
        error: stateQuery.error,
        actionError,
        clearActionError: () => setActionError(null),
        connect: () => setConnectOpen(true),
        refresh,
        sync,
        cancelSync,
        active,
      }}
    >
      {children}
      <AccountDialog
        open={connectOpen}
        onDismiss={() => setConnectOpen(false)}
        onConnected={async (account) => {
          await refresh();
          void sync(account.id);
        }}
      />
    </MailContext.Provider>
  );
}

export function MailGate({ children }: { children: ReactNode }) {
  const mail = useMail();
  const unlock = useMutation({
    mutationFn: unlockMail,
    onSuccess: mail.refresh,
  });
  if (mail.loading)
    return (
      <div className="page mail-empty">
        <Icon name="refresh" className="spinning" size={25} />
        <h1>Opening your mail…</h1>
        <p>Checking the encrypted store on this device.</p>
      </div>
    );
  if (!mail.desktop)
    return (
      <div className="page mail-empty">
        <Icon name="monitor" size={32} />
        <h1>Open the desktop app</h1>
        <p>
          Mail connections and encrypted storage are available in Pigeon’s
          desktop app.
        </p>
        <Link className="button button-secondary" to="/">
          Back to overview
        </Link>
      </div>
    );
  if (mail.error && errorCode(mail.error) === "key_missing")
    return (
      <div className="page mail-empty">
        <Icon name="lock" size={32} />
        <h1>This device’s mail key is missing</h1>
        <p role="alert">
          The keyring no longer has the key for Pigeon’s encrypted store, so
          local-only data cannot be read. Restore a backup to recover it. Mail
          on your servers is unaffected.
        </p>
        <div className="inline-actions">
          <Link className="button button-primary" to="/settings" hash="backup">
            Restore from a backup
          </Link>
        </div>
      </div>
    );
  if (mail.error)
    return (
      <div className="page mail-empty">
        <Icon name="lock" size={32} />
        <h1>Your mail store is unavailable</h1>
        <p role="alert">{errorMessage(mail.error)}</p>
        <div className="inline-actions">
          <button
            className="button button-primary"
            type="button"
            onClick={() => unlock.mutate()}
            disabled={unlock.isPending}
          >
            {unlock.isPending ? "Unlocking…" : "Unlock store"}
          </button>
          <button
            className="button button-secondary"
            type="button"
            onClick={() => {
              void mail.refresh();
            }}
          >
            Try again
          </button>
        </div>
        {unlock.isError && <p role="alert">{errorMessage(unlock.error)}</p>}
      </div>
    );
  return children;
}
