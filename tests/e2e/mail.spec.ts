import { expect, test } from "@playwright/test";

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
          switch (command) {
            case "mail_state":
              if (localStorage.getItem("pigeon-test-locked")) throw { code: "locked", message: "The desktop keyring is locked." };
              return state;
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
              let messages = state.accounts.length ? summaries : [];
              if (query.text?.toLowerCase().includes("receipt"))
                messages = messages.filter((message) => message.id === 1);
              else if (query.text) messages = [];
              if (query.unread_only)
                messages = messages.filter((message) => message.unread);
              if (query.has_attachments) messages = [];
              return { messages, total: messages.length };
            }
            case "read_mail_message":
              return {
                summary: summaries.find((message) => message.id === args.id),
                body_text:
                  localStorage.getItem("pigeon-test-newbody")
                    ? "Updated cached body after sync"
                    : args.id === 1
                    ? "Please find the café receipt. <script>window.mailInjected=true</script>"
                    : "The delivery plan is ready.",
                has_html: false,
                attachments: [],
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
            case "open_mail_message":
            case "update_mail_password":
              return;
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
