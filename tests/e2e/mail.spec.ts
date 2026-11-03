import { expect, type Page, test } from "@playwright/test";

// Deliberate native IPC boundary fixture. Real TLS, IMAP, SQLCipher and keyring
// behaviour are exercised separately by the Rust integration/native suites.
test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => {
    const read = () =>
      JSON.parse(
        localStorage.getItem("pigeon-fixture-state") ??
          '{"accounts":[],"folders":[],"saved_searches":[]}',
      );
    const write = (state: unknown) =>
      localStorage.setItem("pigeon-fixture-state", JSON.stringify(state));
    const summaries = [
      {
        id: 1,
        account_id: "acc-1",
        folder: "INBOX",
        sender: "Alice <alice@example.test>",
        recipients: "mail@example.test",
        subject: "Invoice 4471",
        preview: "Please find the cafe receipt.",
        date: 1793005200,
        unread: true,
        has_attachments: false,
      },
      {
        id: 2,
        account_id: "acc-1",
        folder: "INBOX",
        sender: "Bob <bob@example.test>",
        recipients: "mail@example.test",
        subject: "Release planning",
        preview: "The delivery plan is ready.",
        date: 1793008800,
        unread: false,
        has_attachments: false,
      },
    ];
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      value: {
        invoke: async (command: string, args: Record<string, any> = {}) => {
          const state = read();
          // Message flags/folders and the change queue persist like the engine's.
          state.messages ??= summaries.map((message) => ({ ...message }));
          state.queue ??= [];
          const messageById = (id: number) =>
            state.messages.find((message: any) => message.id === id);
          switch (command) {
            case "mail_state": {
              if (localStorage.getItem("pigeon-test-locked")) throw { code: "locked", message: "The desktop keyring is locked." };
              const count = (states: string[]) =>
                state.queue.filter((action: any) => states.includes(action.state)).length;
              return {
                accounts: state.accounts,
                folders: state.folders,
                saved_searches: state.saved_searches,
                managed_warnings: state.managed_warnings ?? [],
                actions: {
                  pending: count(["queued", "applying"]),
                  failed: count(["failed"]),
                  conflict: count(["conflict"]),
                },
              };
            }
            case "desktop_settings":
              return {
                settings: {
                  closeToBackground: true,
                  notifications: "senderSubject",
                  tray: false,
                  senderImages: !!localStorage.getItem("pigeon-test-sender-images"),
                  remoteImages: !!localStorage.getItem("pigeon-test-remote-images"),
                },
                managed: false,
                warning: null,
              };
            case "set_desktop_settings":
              if (args.settings.senderImages) localStorage.setItem("pigeon-test-sender-images", "on");
              else localStorage.removeItem("pigeon-test-sender-images");
              if (args.settings.remoteImages) localStorage.setItem("pigeon-test-remote-images", "on");
              else localStorage.removeItem("pigeon-test-remote-images");
              return { settings: args.settings, managed: false, warning: null };
            case "sender_image": {
              const looked = JSON.parse(localStorage.getItem("pigeon-test-looked-up") ?? "[]");
              localStorage.setItem("pigeon-test-looked-up", JSON.stringify([...looked, args.domain]));
              return localStorage.getItem("pigeon-test-sender-image");
            }
            case "change_mail_folder": {
              if (localStorage.getItem("pigeon-test-offline")) throw { code: "connection", message: "The mail server could not be reached. Folder changes require a connection." };
              const change = args.change;
              if (change.type === "create") {
                if (state.folders.some((f: any) => f.account_id === args.id && f.name === change.name)) throw { code: "invalid_action", message: "That folder already exists or is reserved." };
                state.folders.push({ id: `folder-${change.name}`, account_id: args.id, name: change.name, kind: "other", delimiter: "/", total: 0, cached: 0, unread: 0 });
              } else {
                const folder = state.folders.find((f: any) => f.id === change.folder_id && f.account_id === args.id);
                if (!folder) throw { code: "not_found", message: "Folder no longer available." };
                if (change.type === "rename") folder.name = change.name;
                else if (change.confirmed) state.folders = state.folders.filter((f: any) => f.id !== folder.id);
              }
              write(state); return;
            }
            case "queue_mail_actions": {
              const kind = args.kind.type;
              const targets = args.ids.map(messageById).filter(Boolean);
              if (!targets.length) throw { code: "not_found", message: "The requested message is no longer available." };
              const target =
                kind === "archive" ? "Archive" : kind === "trash" ? "Trash"
                : kind === "move" ? state.folders.find((folder: any) => folder.id === args.kind.folder_id)?.name
                : null;
              for (const message of targets) {
                if (kind === "mark_read" || kind === "mark_unread") message.unread = kind === "mark_unread";
                else if (localStorage.getItem("pigeon-test-offline")) message.hidden = true;
                else if (kind === "delete_permanently") message.deleted = true;
                else message.folder = target;
              }
              if (localStorage.getItem("pigeon-test-offline"))
                state.queue.push({
                  id: state.queue.length + 1, account_id: "acc-1", kind, folder: targets[0].folder,
                  target, subject: targets[0].subject, count: targets.length, conflicts: 0,
                  state: "queued", error: "the mail server could not be reached or the connection was interrupted",
                  created: 1793008800, updated: 1793008800, ids: args.ids,
                });
              write(state);
              return targets.length;
            }
            case "mail_actions":
              if (localStorage.getItem("pigeon-test-conflict"))
                for (const action of state.queue)
                  if (action.kind === "archive" && action.state === "queued") {
                    action.state = "conflict";
                    action.conflicts = action.count;
                    action.error = `${action.count} message(s) were removed or moved on the server before this change was applied`;
                    for (const id of action.ids) messageById(id).hidden = false;
                  }
              write(state);
              return state.queue;
            case "retry_mail_action": {
              const action = state.queue.find((item: any) => item.id === args.id);
              action.state = "queued";
              action.error = null;
              localStorage.removeItem("pigeon-test-conflict");
              write(state);
              return;
            }
            case "discard_mail_action": {
              const action = state.queue.find((item: any) => item.id === args.id);
              for (const id of action.ids) {
                const message = messageById(id);
                message.hidden = false;
                if (action.kind === "mark_read") message.unread = true;
              }
              state.queue = state.queue.filter((item: any) => item.id !== args.id);
              write(state);
              return;
            }
            case "add_mail_account": {
              if (args.password !== "frontend-test-password")
                throw {
                  code: "authentication",
                  message: "The mail server rejected your sign-in.",
                };
              const account = {
                id: "acc-1",
                config: args.config,
                status: "idle",
                error: null,
                last_synced: null,
                sync_folder: null,
                sync_cached: 0,
                sync_total: 0,
              };
              state.accounts.push(account);
              write(state);
              return account;
            }
            case "sync_mail_account":
              if (state.incoming?.length) {
                state.messages.push(...state.incoming);
                state.incoming = [];
                state.thread_complete = true;
              }
              state.accounts[0].last_synced = 1793008800;
              state.folders = [
                {
                  id: "folder-1",
                  account_id: "acc-1",
                  name: "INBOX",
                  kind: "inbox",
                  total: 2,
                  cached: 2,
                  unread: 1,
                },
              ];
              write(state);
              return;
            case "search_mail": {
              const query = args.query;
              if (query.text === "is:banana") throw { code: "invalid_search", message: "Use is:read or is:unread." };
              let messages = state.accounts.length
                ? state.messages.filter((message: any) => !message.hidden && !message.deleted)
                : [];
              if (query.folder)
                messages = messages.filter((message: any) => message.folder === query.folder);
              const folderSearch = query.text.match(/^folder:(?:"([^"]+)"|(\S+))$/i);
              const context_include_junk_trash = !!query.include_junk_trash || !!query.folder || !!folderSearch;
              if (folderSearch) messages = messages.filter((message: any) => message.folder.toLowerCase().includes((folderSearch[1] ?? folderSearch[2]).toLowerCase()));
              if (!context_include_junk_trash) messages = messages.filter((message: any) => !["Trash", "Junk"].includes(message.folder));
              if (query.text?.toLowerCase().includes("receipt"))
                messages = messages.filter((message: any) => message.id === 1);
              else if (query.text && !folderSearch) messages = [];
              if (query.unread_only)
                messages = messages.filter((message: any) => message.unread);
              if (query.has_attachments) messages = [];
              if (query.conversations) {
                const groups: Record<number, number[]> = {};
                const group_messages: Record<number, any[]> = {};
                const rows: any[] = [];
                for (const message of [...messages].sort((a: any,b: any) => b.date-a.date)) {
                  const row = rows.find((r: any) => message.thread && r.thread === message.thread && r.account_id === message.account_id);
                  if (row) { groups[row.id]!.push(message.id); group_messages[row.id]!.push(message); }
                  else { rows.push({...message}); groups[message.id] = [message.id]; group_messages[message.id] = [message]; }
                }
                return {messages: rows, groups, group_messages, total: rows.length, context_include_junk_trash};
              }
              return { messages, total: messages.length, context_include_junk_trash };
            }
            case "read_mail_conversation": {
              if (localStorage.getItem("pigeon-test-thread-error")) throw { code: "not_found", message: "Message no longer cached." };
              const selected = messageById(args.id);
              return { messages: state.messages.filter((m: any) => !m.hidden && !m.deleted && m.account_id === selected.account_id && (args.includeJunkTrash || !["Trash", "Junk"].includes(m.folder)) && (selected.thread ? m.thread === selected.thread : m.id === selected.id)).sort((a: any,b: any) => a.date-b.date), incomplete: !!selected.thread && !state.thread_complete };
            }
            case "read_mail_message":
              return {
                summary: messageById(args.id),
                body_text: messageById(args.id).body_text ?? (
                  localStorage.getItem("pigeon-test-newbody")
                    ? "Updated cached body after sync"
                    : args.id === 1
                    ? "Please find the café receipt. <script>window.mailInjected=true</script>"
                    : "The delivery plan is ready."),
                has_html: args.id === 2,
                attachments:
                  args.id === 2
                    ? [
                        { part: "1.2.2", filename: "Unnamed attachment", content_type: "image/png", size: 120, content_id: "logo@example.test" },
                        { part: "2", filename: "plan.pdf", content_type: "application/pdf", size: 4096, content_id: null },
                      ]
                    : [],
              };
            case "save_mail_search": {
              const saved = {
                id: "saved-1",
                name: args.name,
                query: args.query,
              };
              state.saved_searches.push(saved);
              write(state);
              return saved;
            }
            case "delete_mail_search":
              state.saved_searches = [];
              write(state);
              return;
            case "remove_mail_account":
              await new Promise((done) => setTimeout(done, 1500));
              state.accounts = [];
              state.folders = [];
              write(state);
              return;
            case "cancel_mail_search":
            case "cancel_mail_sync":
            case "unlock_mail":
              localStorage.removeItem("pigeon-test-locked");
              return;
            case "open_mail_message": {
              // Message 2 models opening while its embedded image is unreachable.
              // Every message has three remote images from two hosts, from news@example.test.
              const trust = JSON.parse(localStorage.getItem("pigeon-test-trust") ?? "{}");
              const sender = "news@example.test";
              const trusted = trust[sender] ?? { all: false, hosts: [] };
              const opens = JSON.parse(localStorage.getItem("pigeon-test-opens") ?? "[]");
              localStorage.setItem("pigeon-test-opens", JSON.stringify([...opens, { id: args.id, loadImages: args.loadImages, loadSources: args.loadSources }]));
              const everything = args.loadImages || trusted.all || !!localStorage.getItem("pigeon-test-remote-images");
              const sources = [["tracker.example", 1], ["cdn.news.example", 2]].map(([host, images]) => {
                const isTrusted = trusted.all || trusted.hosts.includes(host);
                return { host, images, trusted: isTrusted, loaded: everything || isTrusted || args.loadSources.includes(host) };
              });
              const blocked = sources.filter((source) => !source.loaded).reduce((sum, source) => sum + (source.images as number), 0);
              return { missing_inline: args.id === 2 ? 1 : 0, blocked_remote: blocked, sources, sender, trusted: trusted.all };
            }
            case "trusted_senders": {
              const trust = JSON.parse(localStorage.getItem("pigeon-test-trust") ?? "{}");
              return Object.entries(trust).map(([email, value]: [string, any]) => ({ email, all: value.all, hosts: value.all ? [] : value.hosts, added: 1793008800 }));
            }
            case "trust_sender": {
              const trust = JSON.parse(localStorage.getItem("pigeon-test-trust") ?? "{}");
              const current = trust["news@example.test"] ?? { all: false, hosts: [] };
              trust["news@example.test"] = args.sources
                ? { all: current.all, hosts: [...new Set([...current.hosts, ...args.sources])] }
                : { all: true, hosts: [] };
              localStorage.setItem("pigeon-test-trust", JSON.stringify(trust));
              return { email: "news@example.test", ...trust["news@example.test"], added: 1793008800 };
            }
            case "distrust_sender": {
              const trust = JSON.parse(localStorage.getItem("pigeon-test-trust") ?? "{}");
              if (args.source && trust[args.email]) trust[args.email].hosts = trust[args.email].hosts.filter((host: string) => host !== args.source);
              if (!args.source || (!trust[args.email]?.all && !trust[args.email]?.hosts.length)) delete trust[args.email];
              localStorage.setItem("pigeon-test-trust", JSON.stringify(trust));
              return;
            }
            case "update_mail_password":
              return;
            case "send_state":
              return { drafts: state.drafts ?? [], outbox: [], undo_send_seconds: 10 };
            case "save_draft": {
              const draft = { ...args.draft, id: args.draft.id ?? "draft-1", updated: Date.now() };
              state.drafts = [draft];
              write(state);
              return draft;
            }
            case "reply_to_message": {
              const original = messageById(args.id);
              const draft = {
                id: "draft-reply",
                account_id: original.account_id,
                to: args.kind === "forward" ? "" : original.sender,
                cc: "",
                bcc: "",
                subject: `${args.kind === "forward" ? "Fwd" : "Re"}: ${original.subject}`,
                body: `\n\nOn Tue, 27 Oct 2026, ${original.sender} wrote:\n> ${original.preview}\n`,
                in_reply_to: "orig-2@example.test",
                references: "orig-2@example.test",
                updated: Date.now(),
              };
              state.drafts = [draft];
              write(state);
              return draft;
            }
            case "quit_app":
              throw new Error("Not available in a frontend test");
            default:
              throw new Error(`Unexpected native test command: ${command}`);
          }
        },
      },
    });
  });
});

test("connect an account, recover from sign-in failure, read and search cached mail", async ({
  page,
}, testInfo) => {
  await page.goto("/");
  await page
    .getByRole("button", { name: "Connect an account", exact: true })
    .click();
  const dialog = page.getByRole("dialog", {
    name: "Connect an account",
    exact: true,
  });
  await dialog.getByLabel("Account name", { exact: true }).fill("Personal");
  await dialog
    .getByLabel("Email address", { exact: true })
    .fill("mail@example.test");
  await dialog
    .getByLabel("IMAP server", { exact: true })
    .fill("imap.example.test");
  await dialog
    .getByLabel("Password or app password", { exact: true })
    .fill("wrong-password");
  await dialog
    .getByRole("button", { name: "Connect account", exact: true })
    .click();
  await expect(dialog.getByRole("alert")).toContainText(
    "rejected your sign-in",
  );
  await expect(
    dialog.getByLabel("Password or app password", { exact: true }),
  ).toHaveValue("");
  await dialog
    .getByLabel("Password or app password", { exact: true })
    .fill("frontend-test-password");
  await dialog
    .getByRole("button", { name: "Connect account", exact: true })
    .click();
  await expect(dialog).not.toBeVisible();
  await page
    .getByRole("link", { name: "Open Personal inbox", exact: true })
    .click();
  await page.getByRole("button", { name: /Invoice 4471/ }).click();
  await expect(
    page.getByRole("heading", { name: "Invoice 4471", exact: true }),
  ).toBeVisible();
  await expect(page.getByText(/Please find the café receipt/)).toBeVisible();
  expect(await page.evaluate(() => "mailInjected" in window)).toBe(false);
  await page.screenshot({ path: testInfo.outputPath("cached-message.png") });
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
  await page
    .getByRole("textbox", { name: "Search mail", exact: true })
    .fill("receipt");
  await expect(
    page.getByRole("button", { name: /Release planning/ }),
  ).toHaveCount(0);
  await page.getByRole("button", { name: "Save search", exact: true }).click();
  const save = page.getByRole("dialog", { name: "Save search", exact: true });
  await save.getByLabel("Search name", { exact: true }).fill("My receipts");
  await save.getByRole("button", { name: "Save", exact: true }).click();
  await page.reload();
  await expect(
    page.getByRole("textbox", { name: "Search mail", exact: true }),
  ).toHaveValue("receipt");
  await expect(
    page.getByRole("link", { name: "My receipts", exact: true }),
  ).toBeVisible();
  expect(page.url()).not.toMatch(/receipt|Personal|example/);
  expect(
    await page.evaluate(() => Object.values(localStorage).join(" ")),
  ).not.toContain("frontend-test-password");
  await page.screenshot({ path: testInfo.outputPath("saved-search.png") });
  await page.getByRole("button", { name: "Delete saved search", exact: true }).click();
  await expect(page.getByRole("link", { name: "My receipts", exact: true })).toHaveCount(0);
  await page.getByRole("textbox", { name: "Search mail", exact: true }).fill("nobody matches this");
  await expect(page.getByRole("heading", { name: "No matching mail", exact: true })).toBeVisible();
  await page.getByRole("link", { name: "Pigeon home", exact: true }).click();
  await page.getByRole("button", { name: "Manage Personal", exact: true }).click();
  await page.getByRole("button", { name: "Remove from this device", exact: true }).click();
  await page.getByRole("button", { name: "Keep account", exact: true }).click();
  await expect(page.getByRole("dialog", { name: "Manage account", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Remove from this device", exact: true }).click();
  await page.getByRole("button", { name: "Remove account", exact: true }).click();
  await expect(page.getByRole("status").filter({ hasText: "2 cached messages left to remove." })).toBeVisible();
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await page.reload();
  await expect(page.getByRole("heading", { name: "Your inbox starts here.", exact: true })).toBeVisible();
});


test("locked store hides decrypted mail, unlock restores it, and invalid searches recover", async ({ page }) => {
  await page.addInitScript(() => {
    if (!localStorage.getItem("pigeon-fixture-state")) localStorage.setItem("pigeon-fixture-state", JSON.stringify({
      accounts: [{ id: "acc-1", config: { name: "Personal", address: "mail@example.test", host: "imap.example.test", port: 993, username: "mail@example.test", security: "tls" }, status: "error", error: "The server is offline", last_synced: 1793008800, sync_folder: null, sync_cached: 2, sync_total: 2 }],
      folders: [{ id: "folder-1", account_id: "acc-1", name: "INBOX", kind: "inbox", total: 2, cached: 2, unread: 1 }], saved_searches: [],
    }));
  });
  await page.goto("/mail");
  await expect(page.getByText(/Cached mail is still available/)).toBeVisible();
  await page.getByRole("button", { name: /Invoice 4471/ }).click();
  await expect(page.getByRole("heading", { name: "Invoice 4471", exact: true })).toBeVisible();
  await page.evaluate(() => localStorage.setItem("pigeon-test-locked", "true"));
  await expect(page.getByRole("heading", { name: "Your mail store is unavailable", exact: true })).toBeVisible({ timeout: 12000 });
  await expect(page.getByText(/café receipt/)).toHaveCount(0);
  await expect(page.getByRole("button", { name: /Invoice 4471/ })).toHaveCount(0);
  await page.getByRole("button", { name: "Unlock store", exact: true }).click();
  await expect(page.getByRole("button", { name: /Invoice 4471/ })).toBeVisible();
  await page.getByRole("button", { name: /Invoice 4471/ }).click();
  await expect(page.getByText(/café receipt/)).toBeVisible();
  await page.evaluate(() => localStorage.setItem("pigeon-test-newbody", "true"));
  await page.getByRole("button", { name: "Refresh mail", exact: true }).click();
  await expect(page.getByText("Updated cached body after sync", { exact: true })).toBeVisible({ timeout: 2000 });
  const search = page.getByRole("textbox", { name: "Search mail", exact: true });
  await search.fill("is:banana");
  await expect(page.getByRole("heading", { name: "Search could not finish", exact: true })).toBeVisible();
  await expect(page.getByRole("alert")).toContainText("Use is:read or is:unread");
  await search.fill("receipt");
  await expect(page.getByRole("button", { name: /Invoice 4471/ })).toBeVisible();
  await expect(page.getByRole("button", { name: /Release planning/ })).toHaveCount(0);
});

test("nested folders show as a collapsible tree", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name.startsWith("narrow"), "the narrow sidebar shows accounts only");
  await page.addInitScript(() => {
    const folder = (id: string, name: string, unread: number, delimiter: string | null = "/") => ({ id, account_id: "acc-1", name, kind: name === "INBOX" ? "inbox" : "other", delimiter, total: unread, cached: unread, unread });
    if (!localStorage.getItem("pigeon-fixture-state")) localStorage.setItem("pigeon-fixture-state", JSON.stringify({
      accounts: [{ id: "acc-1", config: { name: "Personal", address: "mail@example.test", host: "imap.example.test", port: 993, username: "mail@example.test", security: "tls" }, status: "idle", error: null, last_synced: 1793008800, sync_folder: null, sync_cached: 0, sync_total: 0 }],
      folders: [
        folder("f-inbox", "INBOX", 1),
        folder("f-gigs", "INBOX/Gigs", 2),
        folder("f-polaris", "INBOX/Gigs/Polaris", 3),
        // The parent is unselectable on the server, so it is not listed.
        folder("f-2024", "Projects.2024", 4, "."),
        folder("f-flat", "Notes/Old", 0, null),
      ],
      saved_searches: [],
    }));
  });
  await page.goto("/mail?account=acc-1&view=inbox");
  const tree = page.getByRole("tree");
  await expect(tree.getByRole("link", { name: /^INBOX/ })).toBeVisible();
  await expect(tree.getByRole("link", { name: /Gigs/ })).toHaveCount(0);
  // Collapsed parents add up their children's unread mail.
  await expect(tree.getByRole("link", { name: "INBOX 6", exact: true })).toBeVisible();
  await tree.getByRole("button", { name: "Expand INBOX", exact: true }).click();
  await expect(tree.getByRole("link", { name: "INBOX 1", exact: true })).toBeVisible();
  await expect(tree.getByRole("link", { name: "Gigs 5", exact: true })).toBeVisible();
  await tree.getByRole("button", { name: "Expand Gigs", exact: true }).click();
  await tree.getByRole("link", { name: "Polaris 3", exact: true }).click();
  await expect(page).toHaveURL(/folder=f-polaris/);
  await expect(tree.getByText("Projects", { exact: true })).toBeVisible();
  await tree.getByRole("button", { name: "Expand Projects", exact: true }).click();
  await expect(tree.getByRole("link", { name: "2024 4", exact: true })).toBeVisible();
  // A folder synced before separators were stored stays flat.
  await expect(tree.getByRole("link", { name: "Notes/Old", exact: true })).toBeVisible();
  // Reloading a nested folder opens the path to it.
  await page.reload();
  await expect(page.getByRole("tree").getByRole("link", { name: "Polaris 3", exact: true })).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath("folder-tree.png") });
  await tree.getByRole("button", { name: "Collapse INBOX", exact: true }).click();
  await expect(tree.getByRole("link", { name: /Polaris/ })).toHaveCount(0);
});

test("bulk changes queue offline, show pending work and conflicts, and permanent delete is confirmed", async ({ page }, testInfo) => {
  const narrow = testInfo.project.name.startsWith("narrow");
  await page.addInitScript(() => {
    if (localStorage.getItem("pigeon-fixture-state")) return;
    const folder = (id: string, name: string, kind: string) => ({ id, account_id: "acc-1", name, kind, delimiter: "/", total: 0, cached: 0, unread: 0 });
    const message = (id: number, subject: string, unread: boolean) => ({ id, account_id: "acc-1", folder: "INBOX", sender: "Alice <alice@example.test>", recipients: "mail@example.test", subject, preview: `${subject} preview`, date: 1793005200 + id * 60, unread, has_attachments: false });
    localStorage.setItem("pigeon-fixture-state", JSON.stringify({
      accounts: [{ id: "acc-1", config: { name: "Personal", address: "mail@example.test", host: "imap.example.test", port: 993, username: "mail@example.test", security: "tls" }, status: "idle", error: null, last_synced: 1793008800, sync_folder: null, sync_cached: 4, sync_total: 4 }],
      folders: [folder("f-inbox", "INBOX", "inbox"), folder("f-archive", "Archive", "archive"), folder("f-trash", "Trash", "trash"), folder("f-projects", "Projects", "other")],
      saved_searches: [],
      messages: [message(1, "Invoice 4471", true), message(2, "Release planning", false), message(3, "Team lunch", true), message(4, "Old newsletter", false)],
      queue: [],
    }));
    localStorage.setItem("pigeon-test-offline", "true");
  });
  await page.goto("/mail?account=acc-1&view=inbox");
  await expect(page.getByText("Read-only", { exact: true })).toHaveCount(0);

  // Opening unread mail marks it read; offline, the change waits in the queue.
  await page.getByRole("button", { name: /Invoice 4471/ }).click();
  await page.getByText("Message actions", {exact:true}).click();
  const readerActions = page.getByRole("article", { name: "Message reader" }).getByRole("toolbar", { name: "Message actions" });
  await expect(readerActions.getByRole("button", { name: "Mark unread", exact: true })).toBeVisible();
  if (!narrow) await expect(page.locator(".pending-link")).toContainText("1");
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
  if (narrow) await page.getByRole("button", { name: "Messages", exact: true }).click();

  await page.getByRole("checkbox", { name: "Select Team lunch", exact: true }).check();
  await page.getByRole("checkbox", { name: "Select Old newsletter", exact: true }).check();
  await expect(page.getByText("2 selected", { exact: true })).toBeVisible();
  const bulk = page.locator(".bulk-actions");
  await page.screenshot({ path: testInfo.outputPath("bulk-selection.png") });
  await bulk.getByRole("button", { name: "Archive", exact: true }).click();
  await expect(page.getByRole("button", { name: /Team lunch/ })).toHaveCount(0);
  await expect(page.getByRole("button", { name: /Old newsletter/ })).toHaveCount(0);
  if (!narrow) {
    await expect(page.locator(".pending-link")).toContainText("2");
    await page.locator(".pending-link").click();
  } else await page.goto("/pending");

  const list = page.getByRole("list", { name: "Pending changes" });
  await expect(list.getByText("Mark 1 message read")).toBeVisible();
  await expect(list.getByText("Archive 2 messages")).toBeVisible();
  await expect(list.getByText(/Will retry when the account reconnects/).first()).toBeVisible();
  // The server changed underneath: the archive cannot be applied safely.
  await page.evaluate(() => localStorage.setItem("pigeon-test-conflict", "true"));
  await expect(list.getByText(/removed or moved on the server/)).toBeVisible({ timeout: 5000 });
  await expect(list.getByText("Conflict", { exact: true })).toBeVisible();
  if (!narrow) await expect(page.locator(".pending-link.needs-attention")).toBeVisible({ timeout: 7000 });
  await page.screenshot({ path: testInfo.outputPath("pending-conflict.png") });
  const archive = list.getByRole("listitem").filter({ hasText: "Archive 2 messages" });
  await archive.getByRole("button", { name: "Retry", exact: true }).click();
  await expect(archive.getByText("Waiting", { exact: true })).toBeVisible();
  await archive.getByRole("button", { name: "Discard", exact: true }).click();
  await expect(list.getByText("Archive 2 messages")).toHaveCount(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);

  // Back online: permanent delete needs explicit confirmation.
  await page.evaluate(() => localStorage.removeItem("pigeon-test-offline"));
  await page.goto("/mail?account=acc-1&view=inbox");
  await expect(page.getByRole("button", { name: /Team lunch/ })).toBeVisible();
  await page.getByRole("checkbox", { name: "Select Old newsletter", exact: true }).check();
  await bulk.getByRole("button", { name: "Delete permanently", exact: true }).click();
  const confirm = page.getByRole("dialog", { name: "Delete permanently", exact: true });
  await expect(confirm).toContainText("cannot be recovered from Trash");
  await confirm.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(page.getByRole("button", { name: /Old newsletter/ })).toBeVisible();
  await bulk.getByRole("button", { name: "Delete permanently", exact: true }).click();
  await confirm.getByRole("button", { name: "Delete permanently", exact: true }).click();
  await expect(page.getByRole("button", { name: /Old newsletter/ })).toHaveCount(0);

  // Keyboard: E archives the open message.
  await page.getByRole("button", { name: /Release planning/ }).click();
  await expect(page.getByRole("heading", { name: "Release planning", exact: true })).toBeVisible();
  if (narrow) {
    // Leaving the reader closes it; the shortcut then acts on the selection.
    await page.getByRole("button", { name: "Messages", exact: true }).click();
    await page.getByRole("checkbox", { name: "Select Release planning", exact: true }).check();
  }
  await page.getByRole("button", { name: /Release planning/ }).focus();
  await page.keyboard.press("e");
  await expect(page.getByRole("button", { name: /Release planning/ })).toHaveCount(0);
  await page.goto("/mail?account=acc-1&folder=f-archive");
  await expect(page.getByRole("button", { name: /Release planning/ })).toBeVisible();
});

test("formatted view reports embedded images it could not download", async ({ page }, testInfo) => {
  await page.addInitScript(() => {
    if (!localStorage.getItem("pigeon-fixture-state")) localStorage.setItem("pigeon-fixture-state", JSON.stringify({
      accounts: [{ id: "acc-1", config: { name: "Personal", address: "mail@example.test", host: "imap.example.test", port: 993, username: "mail@example.test", security: "tls" }, status: "idle", error: null, last_synced: 1793008800, sync_folder: null, sync_cached: 2, sync_total: 2 }],
      folders: [{ id: "folder-1", account_id: "acc-1", name: "INBOX", kind: "inbox", delimiter: "/", total: 2, cached: 2, unread: 1 }], saved_searches: [],
    }));
  });
  await page.goto("/mail");
  await page.getByRole("button", { name: /Release planning/ }).click();
  await expect(page.getByText("plan.pdf")).toBeVisible();
  await expect(page.getByText(/Embedded images appear in the formatted view/)).toBeVisible();
  await page.getByRole("button", { name: "Open formatted view", exact: true }).click();
  await expect(page.getByText(/could not be downloaded/)).toHaveText(
    "1 embedded image could not be downloaded, so the formatted view shows its description instead. Reconnect and open it again to try once more.",
  );
  await page.screenshot({ path: testInfo.outputPath("missing-embedded-image.png") });
  // The note belongs to that opening; choosing another message clears it.
  const back = page.getByRole("button", { name: "Messages", exact: true });
  if (await back.isVisible()) await back.click();
  await page.getByRole("button", { name: /Invoice 4471/ }).click();
  await expect(page.getByRole("heading", { name: "Invoice 4471", exact: true })).toBeVisible();
  await expect(page.getByText(/could not be downloaded/)).toHaveCount(0);
});

test("remote images load all, chosen sources, or from trusted senders", async ({ page }, testInfo) => {
  await page.addInitScript(() => {
    if (!localStorage.getItem("pigeon-fixture-state")) localStorage.setItem("pigeon-fixture-state", JSON.stringify({
      accounts: [{ id: "acc-1", config: { name: "Personal", address: "mail@example.test", host: "imap.example.test", port: 993, username: "mail@example.test", security: "tls" }, status: "idle", error: null, last_synced: 1793008800, sync_folder: null, sync_cached: 2, sync_total: 2 }],
      folders: [{ id: "folder-1", account_id: "acc-1", name: "INBOX", kind: "inbox", delimiter: "/", total: 2, cached: 2, unread: 1 }], saved_searches: [],
    }));
  });
  await page.goto("/mail");
  await page.getByRole("button", { name: /Release planning/ }).click();
  const open = page.getByRole("button", { name: "Open formatted view", exact: true });
  await open.click();
  const note = page.getByText(/remote images? from/);
  await expect(note).toHaveText(
    "3 remote images from 2 sources were not loaded. Loading images tells those servers that you opened this message.",
  );
  await page.screenshot({ path: testInfo.outputPath("remote-images-blocked.png") });
  const opens = () => page.evaluate(() => JSON.parse(localStorage.getItem("pigeon-test-opens") ?? "[]"));

  // Choose one source and load it this once.
  await page.getByRole("button", { name: "Choose sources…" }).click();
  const sources = page.getByRole("group", { name: "Image sources" });
  await expect(sources.getByRole("checkbox")).toHaveCount(2);
  await expect(page.getByRole("button", { name: "Load selected", exact: true })).toBeDisabled();
  await sources.getByRole("checkbox", { name: /cdn\.news\.example/ }).check();
  await page.screenshot({ path: testInfo.outputPath("remote-images-choose.png") });
  await page.getByRole("button", { name: "Load selected", exact: true }).click();
  await expect(note).toHaveText(/^1 remote image from 1 source was not loaded/);
  expect((await opens()).at(-1)).toMatchObject({ loadImages: false, loadSources: ["cdn.news.example"] });

  // Trust the other source for this sender; the once-loaded one stays loaded.
  await page.getByRole("button", { name: "Choose sources…" }).click();
  await sources.getByRole("checkbox", { name: /tracker\.example/ }).check();
  await page.getByRole("button", { name: "Always load selected from news@example.test" }).click();
  await expect(note).toHaveCount(0);
  expect((await opens()).at(-1)).toMatchObject({ loadSources: ["cdn.news.example"] });
  await expect(page.getByText("From news@example.test, images from tracker.example always load.")).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath("remote-images-trusted-source.png") });

  // Reopening loads only the trusted source; Load all images is this once.
  await open.click();
  await expect(note).toHaveText(/^2 remote images from 1 source were not loaded/);
  await page.getByRole("button", { name: "Load all images", exact: true }).click();
  await expect(note).toHaveCount(0);
  expect((await opens()).at(-1)).toMatchObject({ loadImages: true });

  // Always load all replaces the chosen sources.
  await open.click();
  await page.getByRole("button", { name: "Always load all from news@example.test" }).click();
  await expect(page.getByText("Images from news@example.test always load.")).toBeVisible();

  await page.goto("/settings");
  const trusted = page.getByRole("region", { name: "Trusted senders" });
  await expect(trusted.getByText("All sources")).toBeVisible();
  await trusted.getByRole("button", { name: "Stop loading images from news@example.test" }).click();
  await expect(trusted.getByText("No trusted senders yet.")).toBeVisible();

  // A sender trusted for chosen sources lists them, each removable.
  await page.evaluate(() => localStorage.setItem("pigeon-test-trust", JSON.stringify({ "news@example.test": { all: false, hosts: ["cdn.news.example", "tracker.example"] } })));
  await page.reload();
  const hosts = trusted.getByRole("list", { name: "Sources trusted for news@example.test" });
  await expect(hosts.getByRole("listitem")).toHaveCount(2);
  await trusted.scrollIntoViewIfNeeded();
  await page.screenshot({ path: testInfo.outputPath("trusted-senders.png") });
  await hosts.getByRole("button", { name: "Stop loading images from tracker.example for news@example.test" }).click();
  await expect(hosts.getByRole("listitem")).toHaveCount(1);
  await expect(hosts).toContainText("cdn.news.example");

  // The global setting loads them everywhere.
  const choice = page.getByRole("group", { name: "Remote images" });
  await choice.getByRole("button", { name: "Always load" }).click();
  await expect(choice.getByRole("button", { name: "Always load" })).toHaveAttribute("aria-pressed", "true");
  await page.goto("/mail");
  await page.getByRole("button", { name: /Release planning/ }).click();
  await page.getByRole("button", { name: "Open formatted view", exact: true }).click();
  await expect(page.getByText(/remote images? from/)).toHaveCount(0);
});

test("a Home Manager account is read-only apart from its password", async ({ page }) => {
  await page.addInitScript(() => {
    if (localStorage.getItem("pigeon-fixture-state")) return;
    localStorage.setItem(
      "pigeon-fixture-state",
      JSON.stringify({
        accounts: [
          {
            id: "managed-personal",
            config: {
              name: "Personal", address: "mail@example.test", host: "imap.example.test", port: 993,
              username: "mail@example.test", security: "tls", ca_certificate: null, managed: true,
              sending: { smtp: { host: "smtp.example.test", port: 587, security: "start_tls", username: "mail@example.test" }, display_name: "Me", signature: "", save_sent_copy: true },
            },
            status: "error", error: "enter this account’s password to start syncing",
            last_synced: null, sync_folder: null, sync_cached: 0, sync_total: 0,
          },
        ],
        folders: [],
        saved_searches: [],
        managed_warnings: ["accounts.json: Account “bad name!” could not be used"],
      }),
    );
  });
  await page.goto("/");
  await expect(page.getByRole("alert").filter({ hasText: "bad name!" }).first()).toBeVisible();
  await expect(page.getByText("enter this account’s password to start syncing").first()).toBeVisible();
  await page.getByRole("button", { name: "Manage Personal", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Manage account", exact: true });
  await expect(dialog.getByText("Managed by Home Manager.")).toBeVisible();
  await expect(dialog.getByText("smtp.example.test:587")).toBeVisible();
  await expect(dialog.getByLabel("Password or app password", { exact: true })).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Remove from this device", exact: true })).toHaveCount(0);
  await expect(dialog.getByText("Sending", { exact: true })).toHaveCount(0);
});

test.describe("sender avatars", () => {
  const PIXEL =
    "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
  const account = {
    id: "acc-1",
    config: { name: "Personal", address: "mail@example.test", host: "imap.example.test", port: 993, username: "mail@example.test", security: "tls" },
    status: "idle", error: null, last_synced: 1793008800, sync_folder: null, sync_cached: 2, sync_total: 2,
  };
  const seed = (page: Page, state: Record<string, unknown>, storage: Record<string, string> = {}) =>
    page.addInitScript(
      ([state, storage]) => {
        if (localStorage.getItem("pigeon-fixture-state")) return;
        localStorage.setItem("pigeon-fixture-state", JSON.stringify(state));
        for (const [key, value] of Object.entries(storage)) localStorage.setItem(key, value);
      },
      [{ accounts: [account], folders: [], saved_searches: [], ...state }, storage] as const,
    );
  const openInbox = async (page: Page) => {
    await page.goto("/");
    await page.getByRole("link", { name: "Open Personal inbox", exact: true }).click();
  };
  const avatar = (page: Page, subject: string) =>
    page.locator(".message-row", { hasText: subject }).locator(".sender-avatar");
  const lookedUp = (page: Page) =>
    page.evaluate(() => JSON.parse(localStorage.getItem("pigeon-test-looked-up") ?? "[]"));

  test("show initials and look nothing up until website icons are chosen", async ({ page }) => {
    await seed(page, {}, { "pigeon-test-sender-image": PIXEL });
    await openInbox(page);
    await expect(avatar(page, "Invoice 4471")).toHaveText("A");
    await expect(avatar(page, "Release planning")).toHaveText("B");
    await page.getByRole("button", { name: /Invoice 4471/ }).click();
    await expect(page.locator(".reader-identity .sender-avatar, .thread-message-active .sender-avatar")).toHaveText("A");
    expect(await lookedUp(page)).toEqual([]);

    await page.goto("/settings");
    const choice = page.getByRole("group", { name: "Sender images" });
    await expect(choice.getByRole("button", { name: "Initials" })).toHaveAttribute("aria-pressed", "true");
    await choice.getByRole("button", { name: "Website icons" }).click();
    await expect(choice.getByRole("button", { name: "Website icons" })).toHaveAttribute("aria-pressed", "true");

    await openInbox(page);
    await expect(avatar(page, "Invoice 4471").locator("img")).toHaveAttribute("src", PIXEL);
    await page.getByRole("button", { name: /Invoice 4471/ }).click();
    await expect(page.locator(".reader-identity .sender-avatar img, .thread-message-active .sender-avatar img")).toHaveAttribute("src", PIXEL);
    // Both senders share a domain, and only the domain leaves the interface.
    expect(await lookedUp(page)).toEqual(["example.test"]);

    await page.goto("/settings");
    await page.getByRole("group", { name: "Sender images" }).getByRole("button", { name: "Initials" }).click();
    await openInbox(page);
    await expect(avatar(page, "Invoice 4471")).toHaveText("A");
    await expect(avatar(page, "Invoice 4471").locator("img")).toHaveCount(0);
  });

  test("keep initials when a sender has no icon or it cannot be shown", async ({ page }) => {
    await seed(page, {}, { "pigeon-test-sender-images": "on" });
    await openInbox(page);
    await expect(avatar(page, "Invoice 4471")).toHaveText("A");
    await expect.poll(() => lookedUp(page)).toEqual(["example.test"]);
    await expect(avatar(page, "Invoice 4471").locator("img")).toHaveCount(0);

    await page.evaluate(() => {
      localStorage.setItem("pigeon-test-sender-image", "data:image/png;base64,AAAA");
    });
    await page.reload();
    await expect(avatar(page, "Invoice 4471")).toHaveText("A");
    await expect(avatar(page, "Invoice 4471").locator("img")).toHaveCount(0);
  });

  test("read names, bare addresses and missing senders", async ({ page }) => {
    const message = (id: number, sender: string, subject: string) => ({
      id, account_id: "acc-1", folder: "INBOX", sender, recipients: "mail@example.test", subject,
      preview: "", date: 1793005200 + id, unread: false, has_attachments: false,
    });
    await seed(page, {
      messages: [
        message(1, '"Ada Lovelace" <ada@example.test>', "Quoted name"),
        message(2, "<first.last@example.test>", "Address alone"),
        message(3, "support@example.test", "Bare address"),
        message(4, "Émile Zola <emile@example.test>", "Accented name"),
        message(5, "Newsletter", "No address"),
        message(6, "", "No sender"),
      ],
    });
    await openInbox(page);
    await expect(avatar(page, "Quoted name")).toHaveText("AL");
    await expect(avatar(page, "Address alone")).toHaveText("FL");
    await expect(avatar(page, "Bare address")).toHaveText("S");
    await expect(avatar(page, "Accented name")).toHaveText("ÉZ");
    await expect(avatar(page, "No address")).toHaveText("N");
    await expect(avatar(page, "No sender")).toHaveText("?");
  });
});

test("reply opens the composer with the quoted message and keeps it threaded", async ({ page }) => {
  await page.addInitScript(() => {
    if (localStorage.getItem("pigeon-fixture-state")) return;
    localStorage.setItem(
      "pigeon-fixture-state",
      JSON.stringify({
        accounts: [
          {
            id: "acc-1",
            config: {
              name: "Personal", address: "mail@example.test", host: "imap.example.test", port: 993,
              username: "mail@example.test", security: "tls", ca_certificate: null,
              sending: { smtp: { host: "smtp.example.test", port: 465, security: "tls", username: "mail@example.test" }, display_name: "Me", signature: "", save_sent_copy: true },
            },
            status: "idle", error: null, last_synced: 1793008800, sync_folder: null, sync_cached: 2, sync_total: 2,
          },
        ],
        folders: [{ id: "folder-1", account_id: "acc-1", name: "INBOX", kind: "inbox", total: 2, cached: 2, unread: 1 }],
        saved_searches: [],
      }),
    );
  });
  await page.goto("/");
  await page.getByRole("link", { name: "Open Personal inbox", exact: true }).click();
  await page.getByRole("button", { name: /Release planning/ }).click();
  await expect(page.getByRole("button", { name: "Forward", exact: true })).toBeVisible();
  await page.getByRole("button", { name: "Reply", exact: true }).click();
  const form = page.getByRole("form", { name: "New message" });
  await expect(form.getByLabel("To", { exact: true })).toHaveValue("Bob <bob@example.test>");
  await expect(form.getByLabel("Subject", { exact: true })).toHaveValue("Re: Release planning");
  await expect(form.getByLabel("Message", { exact: true })).toHaveValue(/> The delivery plan is ready\./);
  await expect(page).toHaveURL(/draft=draft-reply/);
  // Editing autosaves; the threading fields must survive the save.
  await form.getByLabel("Message", { exact: true }).fill("Sounds good.");
  await expect(form.getByRole("status")).toHaveText("Draft saved on this device");
  const saved = await page.evaluate(() => JSON.parse(localStorage.getItem("pigeon-fixture-state")!).drafts[0]);
  expect(saved.in_reply_to).toBe("orig-2@example.test");
  expect(saved.references).toBe("orig-2@example.test");
});

test("conversations expose offline context, preserve flat search and isolate message actions", async ({ page }, testInfo) => {
  await page.addInitScript(() => {
    if (localStorage.getItem("pigeon-fixture-state")) return;
    const base = { account_id: "acc-1", recipients: "mail@example.test", has_attachments: false, thread: "missing-root", preview: "Cached conversation", unread: true };
    localStorage.setItem("pigeon-fixture-state", JSON.stringify({
      accounts: [{ id: "acc-1", config: { name: "Personal", address: "mail@example.test", host: "imap.example.test", port: 993, username: "mail@example.test", security: "tls" }, status: "error", error: "Offline", sync_cached: 3, sync_total: 3 }],
      folders: [{ id: "folder-1", account_id: "acc-1", name: "INBOX", kind: "inbox", total: 2, cached: 2, unread: 2 }, {id: "sent", account_id: "acc-1", name: "Sent", kind: "sent", total: 1, cached: 1, unread: 0}],
      saved_searches: [], messages: [
        {...base, id: 1, folder: "INBOX", sender: "Alice", subject: "Invoice 4471", date: 1793005200, body_text: "Please find the café receipt.\n\nThe total came to £42.80. Let me know if you need anything else for the expense report.\n\nThanks,\nAlice"},
        {...base, id: 2, folder: "INBOX", sender: "Bob", subject: "Re: Invoice 4471", date: 1793008800},
        {...base, id: 3, folder: "Sent", sender: "Me", subject: "Re: Invoice 4471", date: 1793010000, unread: false},
      ],
    }));
    localStorage.setItem("pigeon-test-offline", "true");
  });
  await page.goto("/mail?view=inbox");
  await expect(page.getByText("1 conversation", {exact:true})).toBeVisible();
  await page.locator(".message-row-open").click();
  const context = page.getByRole("region", {name: "Conversation messages"});
  await expect(context.getByRole("button", {name:/Read message from/})).toHaveCount(3);
  await expect(context.getByRole("button", {name:"Read message from Bob",exact:true}).locator(".thread-unread")).toHaveCount(0, {timeout:2000});
  await expect(context.getByText("Part of this conversation is missing")).toBeVisible();
  await context.getByRole("button", {name: "Read message from Alice", exact:true}).click();
  await expect(page.locator(".message-text")).toContainText("café receipt");
  await page.getByText("Message actions", {exact:true}).click();
  await page.getByRole("toolbar", {name: "Message actions"}).getByRole("button", {name: "Mark unread"}).click();
  await expect.poll(() => page.evaluate(() => JSON.parse(localStorage.getItem("pigeon-fixture-state")!).messages.find((m: any) => m.id === 1).unread)).toBe(true);
  expect(await page.evaluate(() => JSON.parse(localStorage.getItem("pigeon-fixture-state")!).messages.find((m: any) => m.id === 3).unread)).toBe(false);
  await expect(context.getByRole("button", {name:"Read message from Alice",exact:true}).locator(".thread-unread")).toHaveCount(1, {timeout:2000});
  await page.evaluate(() => {
    const state = JSON.parse(localStorage.getItem("pigeon-fixture-state")!);
    state.incoming = [{...state.messages[0], id:4, sender:"Earlier sender",date:1793000000, unread:false,body_text:"Recovered ancestor"}];
    localStorage.setItem("pigeon-fixture-state", JSON.stringify(state));
  });
  await page.getByRole("button", {name:"Refresh mail",exact:true}).click();
  await expect(context.getByRole("button", {name:/Read message from/})).toHaveCount(4, {timeout:2000});
  await expect(context.getByText("Part of this conversation is missing")).toHaveCount(0);
  // Queue completion observed by the provider's background poll must refresh context too.
  await page.evaluate(() => {
    const state = JSON.parse(localStorage.getItem("pigeon-fixture-state")!);
    state.queue = [];
    state.messages = state.messages.filter((message:any) => message.id !== 4);
    state.thread_complete = false;
    localStorage.setItem("pigeon-fixture-state", JSON.stringify(state));
  });
  await expect(context.getByRole("button", {name:/Read message from/})).toHaveCount(3, {timeout:8000});
  expect(await page.locator(".message-reader").evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
  await page.getByText("Message actions", {exact:true}).click();
  const body = await page.locator(".message-text").boundingBox();
  expect(body).not.toBeNull();
  expect(body!.y + 25).toBeLessThan(testInfo.project.use.viewport!.height);
  await page.screenshot({path: testInfo.outputPath("conversation-offline.png")});
  await context.getByRole("button", {name: "Read message from Me", exact:true}).click();
  await expect(page.locator(".thread-message-active")).toContainText("Sent");
  await context.getByRole("button", {name: "Read message from Me", exact:true}).click();
  await expect(page.locator(".message-text")).toHaveCount(0);
  await context.getByRole("button", {name: "Read message from Alice", exact:true}).click();
  await expect(page.locator(".message-text")).toContainText("café receipt");
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth)).toBe(true);
  if (testInfo.project.name.startsWith("narrow")) await page.getByRole("button", {name:"Messages",exact:true}).click();
  await page.getByLabel("Mail display").selectOption("messages");
  await expect(page.locator(".message-row-open")).toHaveCount(2);
  await page.getByLabel("Search mail").fill("receipt");
  await expect(page.locator(".message-row-open")).toHaveCount(1);
  await page.getByLabel("Mail display").selectOption("conversations");
  await page.locator(".message-row-open").click();
  await expect(context.getByRole("button", {name:/Read message from/})).toHaveCount(3);
  await page.evaluate(() => localStorage.setItem("pigeon-test-thread-error", "true"));
  await page.reload();
  await page.locator(".message-row-open").click();
  await expect(context.getByRole("alert")).toContainText("Message no longer cached");
  await expect(page.locator(".message-text")).toBeVisible();
});

test("long conversations keep the open body visible and earlier and later messages reachable", async ({page}, testInfo) => {
  await page.addInitScript(() => {
    if (localStorage.getItem("pigeon-fixture-state")) return;
    localStorage.setItem("pigeon-fixture-state", JSON.stringify({
      accounts: [{id:"acc-1", config:{name:"Personal",address:"mail@example.test",host:"imap.example.test",port:993,username:"mail@example.test",security:"tls"},status:"idle"}],
      folders: [{id:"inbox",account_id:"acc-1",name:"INBOX",kind:"inbox",total:15,cached:15,unread:0}], saved_searches:[],
      messages: Array.from({length:15}, (_,i) => ({id:i+1,account_id:"acc-1",folder:"INBOX",sender:`Person ${i+1}`,recipients:"mail@example.test",subject:"Design review",preview:`Detail for message ${i+1}`,body_text:`Detail for message ${i+1}`,date:1793005200+i*60,unread:false,has_attachments:false,thread:"review"})),
    }));
  });
  await page.goto("/mail?view=inbox");
  await page.locator(".message-row-open").click();
  const thread = page.getByRole("region", {name:"Conversation messages",exact:true});
  await expect(page.locator(".message-text")).toHaveText("Detail for message 15");
  await expect(thread.getByText("12 earlier messages", {exact:true})).toBeVisible();
  const body = await page.locator(".message-text").boundingBox();
  expect(body!.y + 20).toBeLessThan(testInfo.project.use.viewport!.height);
  await thread.getByText("12 earlier messages", {exact:true}).click();
  await thread.getByRole("button",{name:"Read message from Person 1",exact:true}).click();
  await expect(page.locator(".message-text")).toHaveText("Detail for message 1");
  await thread.getByText("12 later messages", {exact:true}).click();
  await thread.getByRole("button",{name:"Read message from Person 15",exact:true}).click();
  await expect(page.locator(".message-text")).toHaveText("Detail for message 15");
  await page.getByRole("region", {name:"Message from Person 15",exact:true}).focus();
  await page.keyboard.press("Shift+U");
  await expect.poll(() => page.evaluate(() => JSON.parse(localStorage.getItem("pigeon-fixture-state")!).messages.find((m:any) => m.id === 15).unread)).toBe(true);
  expect(await page.locator(".message-reader").evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
  await page.screenshot({path:testInfo.outputPath("long-conversation.png")});
});

test("server folders validate, create, rename, confirm deletion and retain state on connection failure", async ({ page }, testInfo) => {
  await page.addInitScript(() => {
    if (!localStorage.getItem("pigeon-fixture-state")) localStorage.setItem("pigeon-fixture-state", JSON.stringify({
      accounts: [{ id: "acc-1", config: { name: "Personal", address: "mail@example.test" }, status: "idle", error: null }],
      folders: [{ id: "inbox", account_id: "acc-1", name: "INBOX", kind: "inbox", delimiter: "/", total: 0, cached: 0, unread: 0 }], saved_searches: [],
    }));
  });
  await page.goto("/mail?account=acc-1");
  const open = async () => {
    const button = page.getByRole("button", { name: "Manage folders", exact: true });
    await button.click();
  };
  await open();
  const dialog = page.getByRole("dialog", { name: "Manage server folders" });
  for (const invalid of [" Bad ", "a*b", "a%b", "a&b"]) {
    await dialog.getByLabel("Folder path", { exact: true }).fill(invalid);
    await expect(dialog.getByRole("button", { name: "Create folder", exact: true })).toBeDisabled();
  }
  await dialog.getByLabel("Folder path", { exact: true }).fill("Projects");
  await dialog.getByRole("button", { name: "Create folder", exact: true }).click();
  await expect(dialog).not.toBeVisible();
  await page.goto("/mail?account=acc-1&folder=folder-Projects");
  await open();
  await dialog.getByLabel("Action", { exact: true }).selectOption("rename");
  await dialog.getByLabel("Folder", { exact: true }).selectOption({ label: "Projects" });
  await dialog.getByLabel("New folder path", { exact: true }).fill("Research");
  await dialog.getByRole("button", { name: "Rename folder", exact: true }).click();
  await expect(dialog).not.toBeVisible();
  // The opaque route identifier survives, so the open folder stays selected.
  await expect(page).toHaveURL(/folder=folder-Projects/);
  await expect(page.getByLabel("Filter by folder")).toHaveValue("folder-Projects");
  await expect(page.getByLabel("Filter by folder").locator("option:checked")).toHaveText(/Research/);
  await open();
  await dialog.getByLabel("Action", { exact: true }).selectOption("delete");
  await dialog.getByLabel("Folder", { exact: true }).selectOption({ label: "INBOX" });
  await expect(dialog.getByRole("alert")).toContainText("Special folders");
  await dialog.getByLabel("Folder", { exact: true }).selectOption({ label: "Research" });
  const remove = dialog.getByRole("button", { name: "Delete permanently", exact: true });
  await expect(remove).toBeDisabled();
  await page.screenshot({ path: testInfo.outputPath("folder-delete-confirmation.png") });
  await dialog.getByLabel("I confirm permanent deletion").check();
  await page.evaluate(() => localStorage.setItem("pigeon-test-offline", "1"));
  await remove.click();
  await expect(dialog.getByRole("alert").last()).toContainText("could not be reached");
  await expect(dialog.getByLabel("Folder", { exact: true }).locator("option", { hasText: "Research" })).toHaveCount(1);
  await page.screenshot({ path: testInfo.outputPath("folder-offline-error.png") });
  await page.evaluate(() => localStorage.removeItem("pigeon-test-offline"));
  await remove.click();
  await expect(dialog).not.toBeVisible();
  await expect(page).toHaveURL(/view=inbox/);
  await page.reload(); await open();
  await dialog.getByLabel("Action", { exact: true }).selectOption("delete");
  await expect(dialog.getByLabel("Folder", { exact: true }).locator("option", { hasText: "Research" })).toHaveCount(0);
});

test("deleting another folder leaves the open one selected", async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem("pigeon-fixture-state", JSON.stringify({
      accounts: [{ id: "acc-1", config: { name: "Personal", address: "mail@example.test" }, status: "idle", error: null }],
      folders: [
        { id: "inbox", account_id: "acc-1", name: "INBOX", kind: "inbox", delimiter: "/", total: 0, cached: 0, unread: 0 },
        { id: "folder-keep", account_id: "acc-1", name: "Keep", kind: "other", delimiter: "/", total: 0, cached: 0, unread: 0 },
        { id: "folder-old", account_id: "acc-1", name: "Old", kind: "other", delimiter: "/", total: 0, cached: 0, unread: 0 },
      ], saved_searches: [],
    }));
  });
  await page.goto("/mail?account=acc-1&folder=folder-keep");
  await page.getByRole("button", { name: "Manage folders", exact: true }).click();
  const dialog = page.getByRole("dialog", { name: "Manage server folders" });
  await dialog.getByLabel("Action", { exact: true }).selectOption("delete");
  await dialog.getByLabel("Folder", { exact: true }).selectOption({ label: "Old" });
  await dialog.getByLabel("I confirm permanent deletion").check();
  await dialog.getByRole("button", { name: "Delete permanently", exact: true }).click();
  await expect(dialog).not.toBeVisible();
  await expect(page).toHaveURL(/folder=folder-keep/);
  await expect(page.getByLabel("Filter by folder").locator("option", { hasText: /^Old$/ })).toHaveCount(0);
});

for (const folder of ["Trash", "Junk"]) {
  test(`explicit folder:${folder} searches keep all matching conversation members accessible`, async ({page}) => {
    await page.addInitScript((folder) => {
      if (localStorage.getItem("pigeon-fixture-state")) return;
      localStorage.setItem("pigeon-fixture-state", JSON.stringify({
        accounts:[{id:"acc-1",config:{name:"Personal",address:"mail@example.test"},status:"idle"}],
        folders:[{id:"folder-1",account_id:"acc-1",name:folder,kind:folder.toLowerCase(),total:2,cached:2,unread:0}],saved_searches:[],
        messages:[1,2].map((id) => ({id,account_id:"acc-1",folder,sender:`Sender ${id}`,recipients:"mail@example.test",subject:"Folder thread",body_text:`Cached message ${id}`,preview:`Cached message ${id}`,date:1793005200+id,unread:false,has_attachments:false,thread:"folder-thread"})),
      }));
    },folder);
    await page.goto("/mail?view=all");
    await expect(page.getByRole("heading",{name:"No mail here yet",exact:true})).toBeVisible();
    await page.getByRole("textbox", {name:"Search mail",exact:true}).fill(`folder:${folder}`);
    await expect(page.getByText("1 conversation",{exact:true})).toBeVisible();
    await page.locator(".message-row-open").click();
    const context = page.getByRole("region",{name:"Conversation messages",exact:true});
    await expect(context.getByRole("button",{name:/Read message from/})).toHaveCount(2);
    await context.getByRole("button",{name:"Read message from Sender 1",exact:true}).click();
    await expect(page.locator(".message-text")).toHaveText("Cached message 1");
  });
}
