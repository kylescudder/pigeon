import { useState } from "react";
import type { MailActionKind, MessageSummary } from "../lib/native";
import { Icon } from "./icon";
import { useMail } from "./mail-provider";
import { Modal } from "./modal";

export type MessageActions = ReturnType<typeof useMessageActions>;

/**
 * Server changes for a set of messages. Changes are queued: the list updates
 * at once, and the Pending changes view shows them until the server confirms.
 * `onDone` receives whether the messages left the current view.
 */
export function useMessageActions(
  targets: MessageSummary[],
  onDone: (hidden: boolean) => void,
) {
  const mail = useMail();
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const folders = mail.state?.folders ?? [];
  const accounts = [...new Set(targets.map((message) => message.account_id))];
  const kindOf = (message: MessageSummary) =>
    folders.find(
      (folder) =>
        folder.account_id === message.account_id &&
        folder.name === message.folder,
    )?.kind ?? "other";
  const everyAccountHas = (kind: string) =>
    accounts.every((account) =>
      folders.some(
        (folder) => folder.account_id === account && folder.kind === kind,
      ),
    );
  const any = targets.length > 0;
  const inTrash = any && targets.every((message) => kindOf(message) === "trash");
  const someInTrash = targets.some((message) => kindOf(message) === "trash");
  const hasTrash = everyAccountHas("trash");
  const sourceFolders = new Set(targets.map((message) => message.folder));
  const run = async (kind: MailActionKind) => {
    if (!any || busy) return;
    setBusy(true);
    try {
      const queued = await mail.act(
        targets.map((message) => message.id),
        kind,
      );
      if (queued)
        onDone(kind.type !== "mark_read" && kind.type !== "mark_unread");
    } finally {
      setBusy(false);
    }
  };
  return {
    count: targets.length,
    busy,
    inTrash,
    can: {
      read: targets.some((message) => message.unread),
      unread: targets.some((message) => !message.unread),
      archive:
        any &&
        everyAccountHas("archive") &&
        !targets.every((message) => kindOf(message) === "archive"),
      trash: any && hasTrash && !someInTrash,
      move: accounts.length === 1,
    },
    trashReason: !hasTrash
      ? "This account has no Trash folder on the server. Use Delete permanently instead."
      : someInTrash
        ? "Already in Trash"
        : "Move to Trash",
    moveFolders:
      accounts.length === 1
        ? folders.filter(
            (folder) =>
              folder.account_id === accounts[0] &&
              !(sourceFolders.size === 1 && sourceFolders.has(folder.name)),
          )
        : [],
    run,
    /** Delete: to Trash, or a confirmed permanent delete for mail already there. */
    remove: () => {
      if (inTrash) setConfirming(true);
      else if (any && hasTrash && !someInTrash) void run({ type: "trash" });
    },
    deletePermanently: () => {
      if (any) setConfirming(true);
    },
    dialog: (
      <Modal
        open={confirming}
        onDismiss={() => setConfirming(false)}
        title="Delete permanently"
        className="form-dialog"
      >
        <h2>
          Delete {targets.length === 1 ? "this message" : `${targets.length} messages`}{" "}
          permanently?
        </h2>
        <p>
          They are removed from the mail server for every device and cannot be
          recovered from Trash. Other mail is not affected.
        </p>
        <div className="modal-actions">
          <button
            type="button"
            className="button button-quiet"
            onClick={() => setConfirming(false)}
          >
            Cancel
          </button>
          <button
            type="button"
            className="button button-danger"
            onClick={() => {
              setConfirming(false);
              void run({ type: "delete_permanently" });
            }}
          >
            Delete permanently
          </button>
        </div>
      </Modal>
    ),
  };
}

export function MessageActionButtons({
  actions,
  compact = false,
}: {
  actions: MessageActions;
  compact?: boolean;
}) {
  const { can, busy } = actions;
  return (
    <div className="message-actions" role="toolbar" aria-label="Message actions">
      {can.read ? (
        <button
          type="button"
          className="button button-quiet"
          disabled={busy}
          onClick={() => void actions.run({ type: "mark_read" })}
          title="Mark read (Shift+I)"
        >
          <Icon name="check" size={13} />
          Mark read
        </button>
      ) : (
        <button
          type="button"
          className="button button-quiet"
          disabled={busy || !can.unread}
          onClick={() => void actions.run({ type: "mark_unread" })}
          title="Mark unread (Shift+U)"
        >
          <Icon name="mail" size={13} />
          Mark unread
        </button>
      )}
      {!compact && can.read && can.unread && (
        <button
          type="button"
          className="button button-quiet"
          disabled={busy}
          onClick={() => void actions.run({ type: "mark_unread" })}
          title="Mark unread (Shift+U)"
        >
          Mark unread
        </button>
      )}
      <button
        type="button"
        className="button button-quiet"
        disabled={busy || !can.archive}
        onClick={() => void actions.run({ type: "archive" })}
        title="Archive (E)"
      >
        <Icon name="archive" size={13} />
        Archive
      </button>
      {actions.inTrash ? (
        <button
          type="button"
          className="button button-danger"
          disabled={busy}
          onClick={actions.deletePermanently}
          title="Delete permanently (Shift+Delete)"
        >
          <Icon name="trash" size={13} />
          Delete permanently
        </button>
      ) : (
        <button
          type="button"
          className="button button-quiet"
          disabled={busy || !can.trash}
          onClick={actions.remove}
          title={`${actions.trashReason} (Delete)`}
        >
          <Icon name="trash" size={13} />
          Delete
        </button>
      )}
      {!compact && (
        <>
          <select
            aria-label="Move to folder"
            value=""
            disabled={busy || !can.move || !actions.moveFolders.length}
            title={can.move ? "Move to folder" : "Select mail from one account to move it"}
            onChange={(event) => {
              if (event.target.value)
                void actions.run({ type: "move", folder_id: event.target.value });
            }}
          >
            <option value="">Move to…</option>
            {actions.moveFolders.map((folder) => (
              <option key={folder.id} value={folder.id}>
                {folder.name}
              </option>
            ))}
          </select>
          {!actions.inTrash && (
            <button
              type="button"
              className="button button-quiet message-action-danger"
              disabled={busy}
              onClick={actions.deletePermanently}
              title="Delete permanently (Shift+Delete)"
            >
              Delete permanently
            </button>
          )}
        </>
      )}
      {actions.dialog}
    </div>
  );
}

/** Keyboard shortcuts for the message list. Returns true when handled. */
export function handleActionKey(
  event: React.KeyboardEvent,
  actions: MessageActions,
): boolean {
  if (event.metaKey || event.ctrlKey || event.altKey || !actions.count)
    return false;
  if (event.key === "e" && !event.shiftKey) {
    if (actions.can.archive) void actions.run({ type: "archive" });
  } else if (event.key === "Delete" && event.shiftKey) {
    actions.deletePermanently();
  } else if (event.key === "Delete" || event.key === "#") {
    actions.remove();
  } else if (event.key === "I" && event.shiftKey) {
    if (actions.can.read) void actions.run({ type: "mark_read" });
  } else if (event.key === "U" && event.shiftKey) {
    if (actions.can.unread) void actions.run({ type: "mark_unread" });
  } else return false;
  event.preventDefault();
  return true;
}
