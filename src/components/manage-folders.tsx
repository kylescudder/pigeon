import { useMutation } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { changeMailFolder, errorMessage, type MailAccount, type MailFolder } from "../lib/native";
import { useMail } from "./mail-provider";
import { Icon } from "./icon";
import { Modal } from "./modal";

export function ManageFolders({ account, folders, viewing }: { account: MailAccount; folders: MailFolder[]; viewing?: string }) {
  const mail = useMail();
  const navigate = useNavigate();
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<"create" | "rename" | "delete">("create");
  const [selected, setSelected] = useState("");
  const [name, setName] = useState("");
  const [confirmed, setConfirmed] = useState(false);
  const folder = folders.find(f => f.id === selected);
  const hasChildren = folder?.delimiter && folders.some(f => f.name.startsWith(folder.name + folder.delimiter));
  const protectedFolder = folder && (folder.kind !== "other" || hasChildren);
  const validName = /^[\x20-\x7e]{1,200}$/.test(name) && !/[&*%]/.test(name) && name.trim() === name;
  const mutation = useMutation({
    mutationFn: () => changeMailFolder(account.id, mode === "create" ? { type: mode, name } : mode === "rename" ? { type: mode, folder_id: selected, name } : { type: mode, folder_id: selected, confirmed }),
    onSuccess: async () => {
      await mail.refresh();
      // Folder IDs survive rename. Deleting the open folder returns to the inbox.
      if (mode === "delete" && selected === viewing) await navigate({ to: "/mail", search: { account: account.id, view: "inbox" } });
      setOpen(false);
    },
    onError: () => mail.refresh(),
  });
  const reset = () => { mutation.reset(); setConfirmed(false); };
  return <>
    <button type="button" className="saved-search-link folder-manager" aria-label="Manage folders" title="Manage folders" onClick={() => { reset(); setOpen(true); }}><Icon name="settings" size={14} /><span className="sidebar-label">Manage folders</span></button>
    <Modal open={open && !!mail.state} title="Manage server folders" className="form-dialog" dismissible={!mutation.isPending} onDismiss={() => setOpen(false)}>
      <h2>Manage server folders</h2>
      <p>{account.config.name} · {account.config.address}</p>
      <p>Requires a connection. Changes are complete only after the server confirms. Offline changes are not queued. After an interrupted result, sync this account before trying again.</p>
      <form onSubmit={event => { event.preventDefault(); mutation.mutate(); }}>
        <label>Action<select aria-label="Action" value={mode} onChange={e => { setMode(e.target.value as typeof mode); reset(); setName(""); }} disabled={mutation.isPending}>
          <option value="create">Create folder</option><option value="rename">Rename folder</option><option value="delete">Delete folder</option>
        </select></label>
        {mode !== "create" && <label>Folder<select aria-label="Folder" value={selected} disabled={mutation.isPending} onChange={e => { setSelected(e.target.value); setName(folders.find(f => f.id === e.target.value)?.name ?? ""); reset(); }}>
          <option value="">Choose a folder</option>{folders.map(f => <option key={f.id} value={f.id}>{f.name}</option>)}
        </select></label>}
        {mode !== "create" && protectedFolder && <p role="alert" className="mail-inline-error">Special folders and folders with children cannot be renamed or deleted. Manage child folders first.</p>}
        {mode !== "delete" && <>
          <label>{mode === "create" ? "Folder path" : "New folder path"}<input value={name} maxLength={200} disabled={mutation.isPending} onChange={e => { setName(e.target.value); reset(); }} /></label>
          <p>Use ASCII characters without &amp;, *, % or surrounding spaces. {folders.find(f => f.delimiter)?.delimiter ? `Hierarchy separator: ${folders.find(f => f.delimiter)?.delimiter}.` : "This server uses a flat folder list."} Rename keeps the same parent.</p>
          {name && !validName && <p role="alert" className="mail-inline-error">Enter 1–200 ASCII characters without &amp;, *, % or surrounding spaces.</p>}
        </>}
        {mode === "delete" && folder && !protectedFolder && <>
          <div className="notice notice-error">Permanently delete “{folder.name}” and all mail in it from the server and this device? This affects other mail clients and cannot be undone. It does not move mail to Trash.</div>
          <label className="checkbox-row"><input type="checkbox" checked={confirmed} disabled={mutation.isPending} onChange={e => setConfirmed(e.target.checked)} />I confirm permanent deletion of this folder and its mail.</label>
        </>}
        {mutation.isError && <p role="alert" className="mail-inline-error">{errorMessage(mutation.error)}</p>}
        <div className="inline-actions">
          <button type="button" className="button button-secondary" disabled={mutation.isPending} onClick={() => setOpen(false)}>Cancel</button>
          <button className={`button ${mode === "delete" ? "button-danger" : "button-primary"}`} type="submit" disabled={mutation.isPending || (mode !== "create" && (!folder || !!protectedFolder)) || (mode === "delete" ? !confirmed : !validName)}>{mutation.isPending ? "Waiting for server…" : mode === "delete" ? "Delete permanently" : mode === "rename" ? "Rename folder" : "Create folder"}</button>
        </div>
      </form>
    </Modal>
  </>;
}
