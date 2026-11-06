import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { useEffect, useRef, useState } from "react";
import { MailGate, useMail } from "../components/mail-provider";
import { Modal } from "../components/modal";
import { Icon } from "../components/icon";
import { useRefreshSend } from "../components/send-state";
import { SenderAvatar } from "../components/sender-avatar";
import {
  handleActionKey,
  MessageActionButtons,
  useMessageActions,
  type MessageActions,
} from "../components/message-actions";
import { validateMailRoute } from "../lib/mail-routing";
import {
  deleteMailSearch,
  errorMessage,
  exportAttachment,
  openMailMessage,
  type LoadImages,
  type OpenedMessage,
  replyToMessage,
  type ReplyKind,
  readMailMessage,
  readMailConversation,
  saveMailSearch,
  searchMail,
  trustSender,
  type MailMessage,
  type MessageSummary,
  type SearchQuery,
} from "../lib/native";

export const Route = createFileRoute("/mail")({
  validateSearch: validateMailRoute,
  component: MailRoute,
});

function listTimestamp(ms: number) {
  const date = new Date(ms);
  const isToday = date.toDateString() === new Date().toDateString();
  return new Intl.DateTimeFormat(
    undefined,
    isToday
      ? { hour: "numeric", minute: "2-digit" }
      : { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" },
  ).format(date);
}

function MailRoute() {
  const mail = useMail();
  const route = Route.useSearch();
  const saved = mail.state?.saved_searches.find(
    (saved) => saved.id === route.saved,
  );
  return (
    <MailGate>
      {route.saved && !saved ? (
        <div className="page mail-empty">
          <Icon name="search" size={30} />
          <h1>Saved search unavailable</h1>
          <p>This saved search may have been removed.</p>
        </div>
      ) : (
        <Mailbox
          key={route.saved ?? "ad-hoc"}
          initialQuery={saved?.query}
          savedName={saved?.name}
        />
      )}
    </MailGate>
  );
}

function Mailbox({
  initialQuery,
  savedName,
}: {
  initialQuery?: SearchQuery;
  savedName?: string;
}) {
  const mail = useMail();
  const route = Route.useSearch();
  const navigate = Route.useNavigate();
  const [text, setText] = useState(initialQuery?.text ?? "");
  const [debounced, setDebounced] = useState(text);
  const [unread, setUnread] = useState(initialQuery?.unread_only ?? false);
  const [attachments, setAttachments] = useState(
    initialQuery?.has_attachments ?? false,
  );
  const [junk, setJunk] = useState(initialQuery?.include_junk_trash ?? false);
  const [conversations, setConversations] = useState(initialQuery?.conversations ?? true);
  const [conversationId, setConversationId] = useState<number | null>(null);
  const [page, setPage] = useState(0);
  // `selected` is the message open in the reader; `checked` is the bulk
  // selection the toolbar and shortcuts act on.
  const [selected, setSelected] = useState<number | null>(null);
  const [checked, setChecked] = useState(new Set<number>());
  const [anchor, setAnchor] = useState<number | null>(null);
  const markedRead = useRef(new Set<number>());
  const [saveOpen, setSaveOpen] = useState(false);
  const [searchName, setSearchName] = useState("");
  const searchInput = useRef<HTMLInputElement>(null);
  const accountId =
    route.account === "all"
      ? undefined
      : (route.account ?? initialQuery?.account_id ?? undefined);
  const account = mail.state?.accounts.find(
    (account) => account.id === accountId,
  );
  const routeFolder = mail.state?.folders.find(
    (folder) => folder.id === route.folder,
  );
  const folder =
    route.folder === "all" || route.view === "all"
      ? undefined
      : (routeFolder?.name ??
        (route.view === "inbox"
          ? "INBOX"
          : (initialQuery?.folder ?? undefined)));
  const query: SearchQuery = {
    conversations,
    text: debounced,
    account_id: accountId,
    folder,
    unread_only: unread,
    has_attachments: attachments,
    include_junk_trash: junk,
    limit: 50,
    offset: page * 50,
  };
  const results = useQuery({
    queryKey: ["mail", "search", query],
    queryFn: ({ signal }) => searchMail(query, signal),
    enabled: mail.desktop && !!mail.state,
    refetchInterval: mail.active.size > 0 || mail.state?.accounts.some((item) => item.status === "syncing") ? 2000 : 30000,
  });
  // Rust reports the effective scope, including folder: terms in the text.
  const contextJunkTrash = results.data?.context_include_junk_trash ?? (junk || !!folder);
  const context = useQuery({
    queryKey: ["mail", "conversation", conversationId, contextJunkTrash],
    queryFn: () => readMailConversation(conversationId!, contextJunkTrash),
    enabled: mail.desktop && !!mail.state && conversations && conversationId !== null,
    refetchInterval: 30000,
    gcTime: 0,
  });
  const openMessage = (id: number) => {
    setSelected(id);
    setConversationId(id);
    formatted.reset();
  };
  const detail = useQuery({
    queryKey: ["mail", "message", selected],
    queryFn: () => readMailMessage(selected!),
    enabled: mail.desktop && !!mail.state && selected !== null,
    // A remote deletion can remove this local row. Refresh the selected body
    // with background sync and discard it when leaving the reader.
    refetchInterval: 5000,
    gcTime: 0,
  });
  const queryClient = useQueryClient();
  // Results carry the message they are for, so a note about remote images
  // never shows against a different message.
  const formatted = useMutation({
    mutationFn: async (load: LoadImages) => ({
      ...(await openMailMessage(selected!, load)),
      id: selected!,
    }),
  });
  /** Sources loaded this once and not trusted, kept when the view reopens. */
  const onceLoaded = () =>
    formatted.data?.id === selected
      ? formatted.data.sources
          .filter((source) => source.loaded && !source.trusted)
          .map((source) => source.host)
      : [];
  const trust = useMutation({
    mutationFn: (sources?: string[]) => trustSender(selected!, sources),
    onSuccess: () => {
      void queryClient.invalidateQueries({ queryKey: ["mail", "trusted-senders"] });
      formatted.mutate({ sources: onceLoaded() });
    },
  });
  const save = useMutation({
    mutationFn: () => saveMailSearch(searchName, { ...query, text }),
    onSuccess: async (saved) => {
      await mail.refresh();
      setSaveOpen(false);
      setSearchName("");
      await navigate({ search: { saved: saved.id } });
    },
  });
  const removeSaved = useMutation({
    mutationFn: () => deleteMailSearch(route.saved!),
    onSuccess: async () => {
      await mail.refresh();
      await navigate({ search: {} });
    },
  });
  const scopedAccounts =
    mail.state?.accounts.filter(
      (item) => !accountId || item.id === accountId,
    ) ?? [];
  const syncing = scopedAccounts.filter(
    (item) => item.status === "syncing" || mail.active.has(item.id),
  );
  const failures = scopedAccounts.filter(
    (item) => item.status === "error" || item.status === "interrupted",
  );
  const total = results.data?.total ?? 0;
  const messages = results.data?.messages ?? [];
  const checkedMessages = messages.filter((message) => checked.has(message.id));
  const matchingIds = (id: number) => results.data?.groups?.[id] ?? [id];
  const bulkIds = checkedMessages.flatMap((message) => matchingIds(message.id));
  const openSummary = detail.data?.summary;
  const afterAction = (ids: number[]) => (hidden: boolean) => {
    if (!hidden) return;
    setChecked(new Set());
    if (selected !== null && ids.includes(selected)) setSelected(null);
  };
  const bulk = useMessageActions(
    // Bulk selection affects matching rows only, never hidden context.
    checkedMessages.flatMap((message) => results.data?.group_messages?.[message.id] ?? [message]),
    afterAction(bulkIds),
  );
  const reader = useMessageActions(
    openSummary ? [openSummary] : [],
    afterAction(openSummary ? [openSummary.id] : []),
  );
  // Opening a message marks it read once. A later explicit Mark unread sticks.
  useEffect(() => {
    if (openSummary && !markedRead.current.has(openSummary.id)) {
      markedRead.current.add(openSummary.id);
      if (openSummary.unread) void mail.act([openSummary.id], { type: "mark_read" });
    }
  }, [openSummary?.id, openSummary?.unread]);
  const toggle = (id: number, range: boolean) => {
    setChecked((previous) => {
      const next = new Set(previous);
      const ids = messages.map((message) => message.id);
      if (range && anchor !== null && ids.includes(anchor)) {
        const from = Math.min(ids.indexOf(anchor), ids.indexOf(id));
        const to = Math.max(ids.indexOf(anchor), ids.indexOf(id));
        for (const between of ids.slice(from, to + 1)) next.add(between);
      } else if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
    setAnchor(id);
  };
  useEffect(() => {
    const timeout = setTimeout(() => setDebounced(text), 200);
    return () => clearTimeout(timeout);
  }, [text]);
  useEffect(() => {
    setPage(0);
    setSelected(null);
    setConversationId(null);
    setChecked(new Set());
  }, [debounced, accountId, folder, unread, attachments, junk, conversations]);
  useEffect(() => {
    searchInput.current?.focus();
  }, []);
  const folderOptions =
    mail.state?.folders.filter(
      (item) => !accountId || item.account_id === accountId,
    ) ?? [];
  const title =
    savedName ??
    account?.config.name ??
    (folder === "INBOX" ? "All inboxes" : "All mail");

  return (
    <section
      className={`mail-page${selected !== null ? " message-selected" : ""}`}
    >
      <header className="mail-heading">
        <div>
          <h1>{title}</h1>
          <p>
            {account ? account.config.address : "Your accounts, together"}
            {folder ? ` / ${folder}` : " / All folders"}
          </p>
        </div>
        <div className="inline-actions">
          <button
            type="button"
            className="button button-secondary"
            disabled={!scopedAccounts.length || !!syncing.length}
            onClick={() => {
              for (const account of scopedAccounts) void mail.sync(account.id);
            }}
          >
            <Icon
              name="refresh"
              size={14}
              className={syncing.length ? "spinning" : undefined}
            />
            Refresh mail
          </button>
        </div>
      </header>
      <div className="mail-search">
        <Icon name="search" size={19} />
        <input
          ref={searchInput}
          aria-label="Search mail"
          value={text}
          onChange={(event) => setText(event.target.value)}
          placeholder="Search your mail…"
          spellCheck={false}
          maxLength={4096}
        />
        {text && (
          <button
            type="button"
            className="icon-button"
            aria-label="Clear search"
            onClick={() => setText("")}
          >
            <Icon name="close" size={15} />
          </button>
        )}
        <button
          type="button"
          className="button button-quiet"
          disabled={!mail.state?.accounts.length || results.isError}
          onClick={() => {
            setSaveOpen(true);
            save.reset();
          }}
        >
          Save search
        </button>
      </div>
      <div className="mail-filters">
        <select aria-label="Mail display" value={conversations ? "conversations" : "messages"}
          onChange={(event) => setConversations(event.target.value === "conversations")}>
          <option value="conversations">Conversations</option>
          <option value="messages">Messages</option>
        </select>
        <select
          aria-label="Filter by account"
          value={accountId ?? "all"}
          onChange={(event) => {
            void navigate({
              search: (previous) => ({
                ...previous,
                account: event.target.value,
                folder: "all",
                view: "all",
              }),
            });
          }}
        >
          <option value="all">All accounts</option>
          {mail.state?.accounts.map((account) => (
            <option key={account.id} value={account.id}>
              {account.config.name}
            </option>
          ))}
        </select>
        <select
          aria-label="Filter by folder"
          value={
            routeFolder?.id ??
            (folder === "INBOX"
              ? "inbox"
              : (folderOptions.find((item) => item.name === folder)?.id ??
                "all"))
          }
          onChange={(event) => {
            void navigate({
              search: (previous) => ({
                ...previous,
                account: folderOptions.find((item) => item.id === event.target.value)?.account_id ?? previous.account,
                folder:
                  event.target.value === "inbox"
                    ? undefined
                    : event.target.value,
                view: event.target.value === "inbox" ? "inbox" : undefined,
              }),
            });
          }}
        >
          <option value="all">All folders</option>
          <option value="inbox">All inboxes</option>
          {folderOptions.map((item) => (
            <option key={item.id} value={item.id}>
              {accountId
                ? item.name
                : `${mail.state?.accounts.find((account) => account.id === item.account_id)?.config.name} / ${item.name}`}
            </option>
          ))}
        </select>
        <label>
          <input
            type="checkbox"
            checked={unread}
            onChange={(event) => setUnread(event.target.checked)}
          />
          Unread
        </label>
        <label>
          <input
            type="checkbox"
            checked={attachments}
            onChange={(event) => setAttachments(event.target.checked)}
          />
          Attachments
        </label>
        <label>
          <input
            type="checkbox"
            checked={junk}
            onChange={(event) => setJunk(event.target.checked)}
          />
          Include junk &amp; trash
        </label>
      </div>
      <details className="mail-search-tips">
        <summary>Search tips</summary>
        <p>
          Combine words and phrases with <code>from:alice</code>,{" "}
          <code>to:team</code>, <code>subject:invoice</code>,{" "}
          <code>folder:Archive</code>, <code>account:Personal</code>,{" "}
          <code>after:2026-01-01</code>, <code>before:2026-12-31</code>,{" "}
          <code>is:unread</code>, or <code>has:attachment</code>. Dates use this
          device’s timezone.
        </p>
      </details>
      {!!syncing.length && (
        <div className="sync-coverage" role="status">
          <Icon name="refresh" size={13} className="spinning" />
          <span>
            Syncing {syncing[0]!.config.name}
            {syncing[0]!.sync_folder
              ? ` / ${syncing[0]!.sync_folder} · ${syncing[0]!.sync_cached.toLocaleString()} of ${syncing[0]!.sync_total.toLocaleString()}`
              : "…"}
            . Search includes downloaded mail.
          </span>
          <button
            type="button"
            onClick={() => {
              for (const account of syncing) void mail.cancelSync(account.id);
            }}
          >
            Stop sync
          </button>
        </div>
      )}
      {!syncing.length && !!failures.length && (
        <div className="sync-coverage sync-warning">
          <Icon name="alert" size={14} />
          <span>
            Sync needs attention. Cached mail is still available; some server
            messages may not be downloaded.
          </span>
        </div>
      )}
      <div className="mail-split">
        <div className="message-list-panel">
          <div className="message-list-heading">
            <label className="message-select-all">
              <input
                type="checkbox"
                aria-label="Select all on this page"
                disabled={!messages.length}
                checked={!!messages.length && checkedMessages.length === messages.length}
                ref={(input) => {
                  if (input)
                    input.indeterminate =
                      bulk.count > 0 && checkedMessages.length < messages.length;
                }}
                onChange={(event) =>
                  setChecked(
                    event.target.checked
                      ? new Set(messages.map((message) => message.id))
                      : new Set(),
                  )
                }
              />
              <span>
                {bulk.count
                  ? `${bulk.count} selected`
                  : results.isFetching
                    ? "Searching…"
                    : `${total.toLocaleString()} ${conversations ? (total === 1 ? "conversation" : "conversations") : (total === 1 ? "message" : "messages")}`}
              </span>
            </label>
            {savedName && (
              <button
                type="button"
                className="icon-button"
                aria-label="Delete saved search"
                title="Delete saved search"
                onClick={() => removeSaved.mutate()}
                disabled={removeSaved.isPending}
              >
                <Icon name="trash" size={14} />
              </button>
            )}
          </div>
          {bulk.count > 0 && (
            <div className="bulk-actions">
              <MessageActionButtons actions={bulk} />
            </div>
          )}
          {results.isError ? (
            <div className="list-empty" role="alert">
              <Icon name="alert" size={24} />
              <h2>Search could not finish</h2>
              <p>{errorMessage(results.error)}</p>
              <button
                className="button button-secondary"
                type="button"
                onClick={() => {
                  void results.refetch();
                }}
              >
                Try again
              </button>
            </div>
          ) : results.isPending ? (
            <div className="list-empty">
              <Icon name="refresh" className="spinning" size={24} />
              <p>Searching the local index…</p>
            </div>
          ) : !messages.length ? (
            <div className="list-empty">
              <Icon name="inbox" size={30} />
              <h2>
                {text || unread || attachments
                  ? "No matching mail"
                  : "No mail here yet"}
              </h2>
              <p>
                {mail.state?.accounts.length
                  ? "Try another search or refresh your accounts."
                  : "Connect an account to start receiving mail."}
              </p>
              {!mail.state?.accounts.length && (
                <button
                  className="button button-primary"
                  type="button"
                  onClick={mail.connect}
                >
                  Connect an account
                </button>
              )}
            </div>
          ) : (
            <div
              className="message-list"
              role="group"
              aria-label="Messages"
              onKeyDown={(event) => {
                if (handleActionKey(event, bulk.count ? bulk : reader))
                  return;
                if (event.key === "x" && (conversations ? conversationId : selected) !== null) {
                  event.preventDefault();
                  toggle((conversations ? conversationId : selected)!, false);
                  return;
                }
                if (!["ArrowDown", "ArrowUp"].includes(event.key)) return;
                event.preventDefault();
                const current = messages.findIndex(
                  (message) => message.id === (conversations ? conversationId : selected),
                );
                const index = Math.max(
                  0,
                  Math.min(
                    messages.length - 1,
                    current + (event.key === "ArrowDown" ? 1 : -1),
                  ),
                );
                const message = messages[index];
                if (message) {
                  openMessage(message.id);
                  document.getElementById(`message-${message.id}`)?.focus();
                }
              }}
            >
              {messages.map((message) => (
                <div
                  key={message.id}
                  className={`message-row${(conversations ? conversationId : selected) === message.id ? " selected" : ""}${checked.has(message.id) ? " checked" : ""}${message.unread ? " unread" : ""}`}
                >
                <input
                  type="checkbox"
                  className="message-check"
                  aria-label={`Select ${message.subject || "(No subject)"}`}
                  checked={checked.has(message.id)}
                  onChange={() => {}}
                  onClick={(event) => toggle(message.id, event.shiftKey)}
                />
                <button
                  type="button"
                  id={`message-${message.id}`}
                  className="message-row-open"
                  aria-label={`${message.subject} — ${message.sender}`}
                  aria-pressed={(conversations ? conversationId : selected) === message.id}
                  onClick={(event) => {
                    if (event.metaKey || event.ctrlKey || event.shiftKey) {
                      toggle(message.id, event.shiftKey);
                      return;
                    }
                    openMessage(message.id);
                    formatted.reset();
                    trust.reset();
                  }}
                >
                  <span className="message-dot" />
                  <SenderAvatar sender={message.sender} />
                  <span className="message-row-content">
                    <span className="message-row-top">
                      <strong>{conversations
                        ? [...new Set((results.data?.group_messages?.[message.id] ?? [message]).map((member) => senderName(member.sender)))].join(", ")
                        : message.sender || "Unknown sender"}</strong>
                      <time
                        dateTime={new Date(message.date * 1000).toISOString()}
                      >
                        {listTimestamp(message.date * 1000)}
                      </time>
                    </span>
                    <span className="message-subject">
                      {message.subject || "(No subject)"}
                      {conversations && matchingIds(message.id).length > 1 && <small className="conversation-count" aria-label={`${matchingIds(message.id).length} matching messages`}>{matchingIds(message.id).length}</small>}
                      {message.has_attachments && (
                        <Icon name="file" size={12} />
                      )}
                    </span>
                    <span className="message-preview">
                      {message.preview || "No text preview"}
                    </span>
                    {!accountId && (
                      <span className="message-account">
                        {
                          mail.state?.accounts.find(
                            (account) => account.id === message.account_id,
                          )?.config.name
                        }{" "}
                        / {message.folder}
                      </span>
                    )}
                  </span>
                </button>
                </div>
              ))}
            </div>
          )}
          <div className="mail-pagination">
            <button
              type="button"
              aria-label="Previous page"
              disabled={page === 0 || results.isFetching}
              onClick={() => {
                setPage(page - 1);
                setSelected(null);
                setChecked(new Set());
              }}
            >
              <Icon name="chevron" size={14} className="chevron-back" />
            </button>
            <span>
              {total
                ? `${page * 50 + 1}–${Math.min((page + 1) * 50, total)} of ${total.toLocaleString()}`
                : "0 messages"}
            </span>
            <button
              type="button"
              aria-label="Next page"
              disabled={(page + 1) * 50 >= total || results.isFetching}
              onClick={() => {
                setPage(page + 1);
                setSelected(null);
                setChecked(new Set());
              }}
            >
              <Icon name="chevron" size={14} />
            </button>
          </div>
        </div>
        <article className="message-reader" aria-label="Message reader">
          {selected === null ? (
            <div className="reader-empty">
              <Icon name="mail" size={36} />
              <h2>A little room to read.</h2>
              <p>Select a message to open it here.</p>
            </div>
          ) : detail.isPending ? (
            <div className="reader-empty">
              <Icon name="refresh" size={24} className="spinning" />
              <p>Opening cached message…</p>
            </div>
          ) : detail.isError ? (
            <div className="reader-empty" role="alert">
              <Icon name="alert" size={26} />
              <h2>Message unavailable</h2>
              <p>{errorMessage(detail.error)}</p>
              <button
                type="button"
                className="button button-secondary"
                onClick={() => setSelected(null)}
              >
                Back to messages
              </button>
            </div>
          ) : detail.data ? (
            conversations ? (
              <ConversationReader
                message={detail.data}
                key={detail.data.summary.id}
                members={context.data?.messages ?? [detail.data.summary]}
                loading={context.isPending}
                contextError={context.error}
                incomplete={context.data?.incomplete ?? false}
                onRetry={() => void context.refetch()}
                onSelect={(id) => { setSelected(id); formatted.reset(); trust.reset(); }}
                actions={reader}
                onBack={() => setSelected(null)}
                onFormatted={() => formatted.mutate({})}
                formattedPending={formatted.isPending || trust.isPending}
                formattedError={formatted.error ?? trust.error}
                opened={formatted.data?.id === detail.data.summary.id ? formatted.data : undefined}
                onLoadImages={(sources) => formatted.mutate(sources ? { sources: [...onceLoaded(), ...sources] } : { all: true })}
                onTrust={(sources) => trust.mutate(sources)}
              />
            ) : (
              <MessageReader
                message={detail.data}
                key={detail.data.summary.id}
                actions={reader}
                onBack={() => setSelected(null)}
                onFormatted={() => formatted.mutate({})}
                formattedPending={formatted.isPending || trust.isPending}
                formattedError={formatted.error ?? trust.error}
                opened={formatted.data?.id === detail.data.summary.id ? formatted.data : undefined}
                onLoadImages={(sources) => formatted.mutate(sources ? { sources: [...onceLoaded(), ...sources] } : { all: true })}
                onTrust={(sources) => trust.mutate(sources)}
              />
            )
          ) : null}
        </article>
      </div>
      {removeSaved.isError && (
        <p className="mail-inline-error" role="alert">
          {errorMessage(removeSaved.error)}
        </p>
      )}
      <Modal
        open={saveOpen}
        onDismiss={() => setSaveOpen(false)}
        title="Save search"
        className="form-dialog"
        dismissible={!save.isPending}
      >
        <h2>Save search</h2>
        <p>Keep these filters in your encrypted mail store.</p>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            save.mutate();
          }}
        >
          <label htmlFor="saved-search-name">
            Search name
            <input
              id="saved-search-name"
              value={searchName}
              onChange={(event) => setSearchName(event.target.value)}
              maxLength={100}
              required
            />
          </label>
          {save.isError && <p role="alert">{errorMessage(save.error)}</p>}
          <div className="modal-actions">
            <button
              type="button"
              className="button button-quiet"
              onClick={() => setSaveOpen(false)}
              disabled={save.isPending}
            >
              Cancel
            </button>
            <button
              type="submit"
              className="button button-primary"
              disabled={save.isPending}
            >
              {save.isPending ? "Saving…" : "Save"}
            </button>
          </div>
        </form>
      </Modal>
    </section>
  );
}

type MessageReaderProps = {
  message: MailMessage;
  actions: MessageActions;
  onBack: () => void;
  onFormatted: () => void;
  formattedPending: boolean;
  formattedError: unknown;
  opened: OpenedMessage | undefined;
  onLoadImages: (sources?: string[]) => void;
  onTrust: (sources?: string[]) => void;
  embedded?: boolean;
};

function senderName(sender: string) {
  return sender.replace(/\s*<[^>]+>\s*$/, "").replace(/^"|"$/g, "") || sender || "Unknown sender";
}

function ConversationReader(props: MessageReaderProps & {
  members: MessageSummary[];
  loading: boolean;
  contextError: unknown;
  incomplete: boolean;
  onRetry: () => void;
  onSelect: (id: number) => void;
}) {
  const { message, members, actions } = props;
  const selected = message.summary.id;
  const [collapsed, setCollapsed] = useState(false);
  const openHeader = useRef<HTMLButtonElement>(null);
  useEffect(() => {
    setCollapsed(false);
    openHeader.current?.scrollIntoView({ block: "nearest" });
  }, [selected]);
  // A compact window around the open message keeps long threads readable.
  // Earlier/later messages remain available through ordinary disclosure controls.
  const all = members.some((member) => member.id === selected)
    ? members : [...members, message.summary].sort((a, b) => a.date - b.date || a.id - b.id);
  const position = all.findIndex((member) => member.id === selected);
  const first = Math.max(0, position - 2);
  const last = Math.min(all.length, position + 3);
  const renderMember = (member: MessageSummary) => {
    const active = member.id === selected;
    const name = senderName(member.sender);
    return (
      <section key={member.id} className={`thread-message${active ? " thread-message-active" : ""}`}>
        <button
          type="button"
          ref={active ? openHeader : undefined}
          className="thread-message-heading"
          aria-label={`Read message from ${name}`}
          aria-expanded={active && !collapsed}
          aria-controls={active ? `thread-body-${member.id}` : undefined}
          onClick={() => {
            if (active) setCollapsed((value) => !value);
            else { setCollapsed(false); props.onSelect(member.id); }
          }}
        >
          <SenderAvatar sender={member.sender} />
          <span className="thread-person">
            <span className="thread-sender">{name}{member.unread && <span className="thread-unread" title="Unread" />}</span>
            <span className="thread-snippet">{active && !collapsed ? `To: ${member.recipients}` : member.preview || member.subject || "No text preview"}</span>
          </span>
          <span className="thread-stamp">
            <time dateTime={new Date(member.date * 1000).toISOString()}>{new Intl.DateTimeFormat(undefined, {month: "short", day: "numeric", hour: "numeric", minute: "2-digit"}).format(member.date * 1000)}</time>
            <span>{member.folder}</span>
          </span>
          <Icon name="chevron" size={13} className={active && !collapsed ? "thread-chevron-open" : undefined} />
        </button>
        {active && !collapsed && (
          <div id={`thread-body-${member.id}`} className="thread-body" tabIndex={0} role="region" aria-label={`Message from ${name}`}>
            <div className="thread-message-tools">
              {message.summary.sender !== senderName(message.summary.sender) && <span>{message.summary.sender}</span>}
              {message.has_html && <button type="button" className="button button-quiet" onClick={props.onFormatted} disabled={props.formattedPending}>{props.formattedPending ? "Opening…" : "Open formatted view"}</button>}
              <details className="thread-action-menu">
                <summary>Message actions</summary>
                <div className="thread-action-popover"><MessageActionButtons actions={actions} /></div>
              </details>
            </div>
            <MessageReader {...props} embedded />
          </div>
        )}
      </section>
    );
  };
  return (
    <div className="conversation-reader" onKeyDown={(event) => {
      if (!(event.target instanceof HTMLElement) || event.target.closest("input,select,textarea,summary,button")) return;
      handleActionKey(event, actions);
    }}>
      <header className="thread-heading">
        <button type="button" className="icon-button thread-back" aria-label="Messages" onClick={props.onBack}><Icon name="chevron" size={16} className="chevron-back" /></button>
        <div><h2>{message.summary.subject.replace(/^(?:(?:re|fwd?):\s*)+/i, "") || "(No subject)"}</h2><p>{all.length} {all.length === 1 ? "message" : "messages"}{props.loading ? " · Loading conversation…" : ""}</p></div>
      </header>
      <div className="thread-timeline" role="region" aria-label="Conversation messages">
        {props.contextError != null && <div className="thread-notice" role="alert">Conversation unavailable. {errorMessage(props.contextError)} <button type="button" onClick={props.onRetry}>Try again</button></div>}
        {props.incomplete && <details className="thread-cache-note"><summary>Part of this conversation is missing</summary><p>Some referenced messages are not cached. Sync this account when you’re online to look for them.</p></details>}
        {first > 0 && <details className="thread-history"><summary>{first} earlier {first === 1 ? "message" : "messages"}</summary>{all.slice(0, first).map(renderMember)}</details>}
        {all.slice(first, last).map(renderMember)}
        {last < all.length && <details className="thread-history"><summary>{all.length - last} later messages</summary>{all.slice(last).map(renderMember)}</details>}
      </div>
    </div>
  );
}

function MessageReader({
  message,
  actions,
  onBack,
  onFormatted,
  formattedPending,
  formattedError,
  opened,
  onLoadImages,
  onTrust,
  embedded = false,
}: MessageReaderProps) {
  const [choosing, setChoosing] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const missingInline = opened?.missing_inline ?? 0;
  const blocked = opened?.blocked_remote ?? 0;
  const blockedSources = opened?.sources.filter((source) => !source.loaded) ?? [];
  const trustedHosts =
    opened?.sources.filter((source) => source.trusted).map((source) => source.host) ?? [];
  const finish = (act: () => void) => {
    act();
    setChoosing(false);
    setPicked(new Set());
  };
  return (
    <>
      {!embedded && <div className="reader-toolbar">
        <button
          className="button button-quiet reader-back"
          type="button"
          onClick={onBack}
        >
          <Icon name="chevron" size={14} className="chevron-back" />
          Messages
        </button>
        {message.has_html && (
          <button
            className="button button-secondary"
            type="button"
            onClick={onFormatted}
            disabled={formattedPending}
          >
            {formattedPending ? "Opening…" : "Open formatted view"}
          </button>
        )}
        <ReplyButtons messageId={message.summary.id} />
        <MessageActionButtons actions={actions} compact />
        <span>Plain text</span>
      </div>}
      <div className="reader-content">
        {!embedded && <h2>{message.summary.subject || "(No subject)"}</h2>}
        {!embedded && <div className="reader-identity">
          <SenderAvatar sender={message.summary.sender} />
          <div>
            <strong>{message.summary.sender || "Unknown sender"}</strong>
            <p>To: {message.summary.recipients}</p>
            <time>
              {new Intl.DateTimeFormat(undefined, {
                dateStyle: "medium",
                timeStyle: "short",
              }).format(message.summary.date * 1000)}
            </time>
          </div>
        </div>
        }
        {formattedError != null && (
          <p role="alert" className="mail-inline-error">
            {errorMessage(formattedError)}
          </p>
        )}
        {missingInline > 0 && (
          <p role="status" className="mail-inline-note">
            {missingInline === 1
              ? "1 embedded image"
              : `${missingInline} embedded images`}{" "}
            could not be downloaded, so the formatted view shows{" "}
            {missingInline === 1 ? "its" : "their"} description instead.
            Reconnect and open it again to try once more.
          </p>
        )}
        {blocked > 0 && (
          <div role="status" className="mail-inline-note remote-images-note">
            <p>
              {blocked === 1 ? "1 remote image" : `${blocked} remote images`}{" "}
              from{" "}
              {blockedSources.length === 1
                ? "1 source"
                : `${blockedSources.length} sources`}{" "}
              {blocked === 1 ? "was" : "were"} not loaded. Loading images tells
              those servers that you opened this message.
            </p>
            <div className="remote-images-actions">
              <button
                type="button"
                className="button button-secondary"
                onClick={() => finish(() => onLoadImages())}
                disabled={formattedPending}
              >
                Load all images
              </button>
              {opened?.sender && (
                <button
                  type="button"
                  className="button button-quiet"
                  onClick={() => finish(() => onTrust())}
                  disabled={formattedPending}
                >
                  Always load all from {opened.sender}
                </button>
              )}
              <button
                type="button"
                className="button button-quiet"
                aria-expanded={choosing}
                aria-controls="image-sources"
                onClick={() => setChoosing(!choosing)}
              >
                Choose sources…
              </button>
            </div>
            {choosing && (
              <fieldset id="image-sources" className="image-sources">
                <legend>Image sources</legend>
                {blockedSources.map((source) => (
                  <label key={source.host}>
                    <input
                      type="checkbox"
                      checked={picked.has(source.host)}
                      onChange={(event) => {
                        const next = new Set(picked);
                        if (event.target.checked) next.add(source.host);
                        else next.delete(source.host);
                        setPicked(next);
                      }}
                    />
                    <span>{source.host}</span>
                    <small>
                      {source.images === 1 ? "1 image" : `${source.images} images`}
                    </small>
                  </label>
                ))}
                <div className="remote-images-actions">
                  <button
                    type="button"
                    className="button button-secondary"
                    disabled={formattedPending || picked.size === 0}
                    onClick={() => finish(() => onLoadImages([...picked]))}
                  >
                    Load selected
                  </button>
                  {opened?.sender && (
                    <button
                      type="button"
                      className="button button-quiet"
                      disabled={formattedPending || picked.size === 0}
                      onClick={() => finish(() => onTrust([...picked]))}
                    >
                      Always load selected from {opened.sender}
                    </button>
                  )}
                </div>
              </fieldset>
            )}
          </div>
        )}
        {opened && (opened.trusted || trustedHosts.length > 0) && (
          <p role="status" className="mail-inline-note">
            {opened.trusted
              ? `Images from ${opened.sender} always load.`
              : `From ${opened.sender}, images from ${trustedHosts.join(", ")} always load.`}{" "}
            <Link to="/settings" hash="remote-images">
              Change in Settings
            </Link>
          </p>
        )}
        <div className="message-text">
          {message.body_text || "This message has no text body."}
        </div>
        {!!message.attachments.length && (
          <section className="message-attachments">
            <h3>
              {message.attachments.length}{" "}
              {message.attachments.length === 1 ? "attachment" : "attachments"}
            </h3>
            {message.attachments.map((attachment) => (
              <div key={attachment.part}>
                <Icon name="file" size={20} />
                <span>
                  {attachment.filename}
                  <small>{attachment.content_type}</small>
                </span>
                <AttachmentActions id={message.summary.id} part={attachment.part} />
              </div>
            ))}
            <p>Saving creates a file outside encrypted storage. “Save and open” also opens it in your default application. Only open files you trust. Forward includes text only; attach files separately.</p>
          </section>
        )}
        {embedded && <footer className="thread-reply"><ReplyButtons messageId={message.summary.id} /></footer>}
      </div>
    </>
  );
}

/** Starts a reply or forward as a draft and opens it in the composer. */
function ReplyButtons({ messageId }: { messageId: number }) {
  const navigate = useNavigate();
  const refreshSend = useRefreshSend();
  const start = useMutation({
    mutationFn: (kind: ReplyKind) => replyToMessage(messageId, kind),
    onSuccess: async (draft) => {
      // The composer finds the draft in the send state, so load it first.
      await refreshSend();
      await navigate({ to: "/compose", search: { draft: draft.id } });
    },
  });
  const choices: [ReplyKind, string][] = [
    ["reply", "Reply"],
    ["reply_all", "Reply all"],
    ["forward", "Forward"],
  ];
  return (
    <>
      {choices.map(([kind, label]) => (
        <button
          key={kind}
          className="button button-secondary"
          type="button"
          onClick={() => start.mutate(kind)}
          disabled={start.isPending}
        >
          {label}
        </button>
      ))}
      {start.isError && (
        <span role="alert" className="mail-inline-error">
          {errorMessage(start.error)}
        </span>
      )}
    </>
  );
}

function AttachmentActions({ id, part }: { id: number; part: string }) {
  const action = useMutation({ mutationFn: (open: boolean) => exportAttachment(id, part, open) });
  return <span className="attachment-actions">
    <button type="button" className="button button-secondary" disabled={action.isPending} onClick={() => action.mutate(false)}>Save</button>
    <button type="button" className="button button-secondary" disabled={action.isPending} onClick={() => action.mutate(true)}>Save and open</button>
    {action.isError && <small role="alert">{errorMessage(action.error)}</small>}
    {action.isSuccess && action.data && <small role="status">Saved outside encrypted storage</small>}
  </span>;
}
