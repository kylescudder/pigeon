import { expect, test, type Page } from "@playwright/test";

// Native IPC boundary fixture for composing and the outbox. Real SMTP
// outcomes, the undo delay and Sent-copy saving are exercised by the Rust
// suites; this checks what the interface shows and asks for.
async function fixture(
  page: Page,
  options: {
    smtp?: boolean;
    managed?: boolean;
    failSendingSave?: boolean;
    undo?: number;
    outcome?: "accepted" | "unknown" | "failed" | "held";
    drafts?: Record<string, string>[];
  },
) {
  await page.addInitScript((options) => {
    const KEY = "pigeon-compose-fixture";
    if (!localStorage.getItem(KEY)) {
      const smtp = options.smtp === false
        ? null
        : { host: "smtp.example.test", port: 465, security: "tls", username: "mail@example.test" };
      localStorage.setItem(
        KEY,
        JSON.stringify({
          accounts: [
            {
              id: "acc-1",
              config: {
                name: "Personal",
                managed: options.managed ?? false,
                address: "mail@example.test",
                host: "imap.example.test",
                port: 993,
                username: "mail@example.test",
                security: "tls",
                ca_certificate: null,
                sending: {
                  smtp,
                  display_name: "Test Person",
                  signature: "Sent with care",
                  save_sent_copy: true,
                },
              },
              status: "idle",
              error: null,
              last_synced: 1793008800,
              sync_folder: null,
              sync_cached: 0,
              sync_total: 0,
            },
          ],
          folders: [],
          saved_searches: [],
          drafts: (options.drafts ?? []).map((draft, index) => ({
            id: `saved-${index + 1}`,
            account_id: "acc-1",
            to: "",
            cc: "",
            bcc: "",
            subject: "",
            body: "",
            updated: 1793008800000,
            ...draft,
          })),
          outbox: [],
          recipients: [],
          undo: options.undo ?? 10,
          next: 1,
        }),
      );
    }
    const read = () => JSON.parse(localStorage.getItem(KEY)!);
    const write = (state: unknown) =>
      localStorage.setItem(KEY, JSON.stringify(state));
    // Stands in for the background worker: due messages get an outcome.
    const settle = (state: any) => {
      for (const item of state.outbox) {
        if (item.send.state === "queued" && item.send.send_at <= Date.now()) {
          item.send =
            options.outcome === "held" ? { state: "held" } : options.outcome === "unknown"
              ? { state: "unknown", error: "the connection ended after the message was sent, before the server confirmed it; it may have been delivered" }
              : options.outcome === "failed"
                ? { state: "failed", error: "the server refused bob@example.test (550 5.1.1)" }
                : { state: "accepted", at: Date.now() };
          if (item.send.state === "accepted") {
            for (const field of [item.draft.to, item.draft.cc, item.draft.bcc]) {
              // Fixture mailbox parsing covers the test data; Rust owns RFC parsing.
              for (const entry of field.split(/,(?=(?:[^"]*"[^"]*")*[^"]*$)/)) {
                const match = entry.trim().match(/^(?:(.*?)\s*<)?([^<>\s]+@[^<>\s]+)>?$/);
                if (!match) continue;
                const email = match[2]!;
                const previous = state.recipients.find((r: any) => r.email.toLowerCase() === email.toLowerCase());
                if (previous) previous.name = match[1]?.replace(/^"|"$/g, "") || previous.name;
                else state.recipients.push({ email, name: match[1]?.replace(/^"|"$/g, "") || null });
              }
            }
          }
          item.sent_copy =
            item.send.state === "accepted" ? { state: "saved" } : { state: "not_requested" };
        }
      }
      // The engine stops listing a send once it needs nothing more.
      state.outbox = state.outbox.filter(
        (item: any) => !(item.send.state === "accepted" && item.sent_copy.state === "saved"),
      );
    };
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      value: {
        invoke: async (command: string, args: Record<string, any> = {}) => {
          const state = read();
          const draftOf = (input: any) => {
            const id = input.id ?? `draft-${state.next++}`;
            const draft = { ...input, id, updated: Date.now() };
            state.drafts = [draft, ...state.drafts.filter((d: any) => d.id !== id)];
            return draft;
          };
          switch (command) {
            case "mail_state":
              return { accounts: state.accounts, folders: state.folders, saved_searches: state.saved_searches };
            case "send_state":
              settle(state);
              write(state);
              return { drafts: state.drafts, outbox: state.outbox, undo_send_seconds: state.undo, recipients: state.recipients };
            case "save_draft": {
              const draft = draftOf(args.draft);
              write(state);
              return draft;
            }
            case "delete_draft":
              state.drafts = state.drafts.filter((d: any) => d.id !== args.id);
              write(state);
              return;
            case "send_draft": {
              const draft = draftOf(args.draft);
              if (!draft.to.trim()) {
                write(state);
                throw { code: "invalid_config", message: "account settings are invalid: add at least one recipient" };
              }
              state.drafts = state.drafts.filter((d: any) => d.id !== draft.id);
              const item = {
                id: `out-${state.next++}`,
                account_id: draft.account_id,
                subject: draft.subject,
                recipients: draft.to,
                created: Date.now(),
                draft,
                send: { state: "queued", send_at: Date.now() + state.undo * 1000, error: null },
                sent_copy: { state: "not_requested" },
              };
              state.outbox.push(item);
              write(state);
              return item;
            }
            case "return_to_drafts": {
              const item = state.outbox.find((i: any) => i.id === args.id);
              if (
                localStorage.getItem("pigeon-test-too-late") ||
                !item ||
                !["queued", "failed"].includes(item.send.state)
              )
                throw { code: "conflict", message: "sending has already started, so it can no longer be undone" };
              state.outbox = state.outbox.filter((i: any) => i.id !== args.id);
              const draft = draftOf(item.draft);
              write(state);
              return draft;
            }
            case "resolve_send": {
              const item = state.outbox.find((i: any) => i.id === args.id);
              if (args.decision === "mark_sent") {
                item.send = { state: "accepted", at: Date.now() };
                if (item.send.state === "accepted") {
            for (const field of [item.draft.to, item.draft.cc, item.draft.bcc]) {
              // Fixture mailbox parsing covers the test data; Rust owns RFC parsing.
              for (const entry of field.split(/,(?=(?:[^"]*"[^"]*")*[^"]*$)/)) {
                const match = entry.trim().match(/^(?:(.*?)\s*<)?([^<>\s]+@[^<>\s]+)>?$/);
                if (!match) continue;
                const email = match[2]!;
                const previous = state.recipients.find((r: any) => r.email.toLowerCase() === email.toLowerCase());
                if (previous) previous.name = match[1]?.replace(/^"|"$/g, "") || previous.name;
                else state.recipients.push({ email, name: match[1]?.replace(/^"|"$/g, "") || null });
              }
            }
          }
          item.sent_copy = { state: "saved" };
              } else if (args.decision === "discard") {
                state.outbox = state.outbox.filter((i: any) => i.id !== args.id);
              } else {
                item.send = { state: "queued", send_at: Date.now(), error: null };
              }
              write(state);
              return;
            }
            case "dismiss_outbox":
              state.outbox = state.outbox.filter((i: any) => i.id !== args.id);
              write(state);
              return;
            case "retry_send":
            case "retry_sent_copy":
              return;
            case "update_mail_sending":
              if (options.failSendingSave)
                throw { code: "storage", message: "Could not write to the encrypted store" };
              const account = state.accounts.find((item: any) => item.id === args.id);
              account.config.sending = args.settings;
              write(state);
              return account;
            case "set_undo_send_seconds":
              state.undo = args.seconds;
              write(state);
              return;
            case "search_mail":
              return { messages: [], total: 0 };
            case "sync_mail_account":
            case "cancel_mail_search":
              return;
            default:
              throw new Error(`Unexpected native test command: ${command}`);
          }
        },
      },
    });
  }, options);
}

test("drafts autosave and survive reload, and undo returns a sent message to the composer", async ({
  page,
}, testInfo) => {
  await fixture(page, {});
  await page.goto("/");
  await page.getByRole("link", { name: "New message", exact: true }).click();
  const form = page.getByRole("form", { name: "New message" });
  await expect(form.getByLabel("From")).toContainText("Test Person <mail@example.test>");
  await form.getByLabel("To", { exact: true }).fill("Alice <alice@example.test>");
  await form.getByRole("button", { name: "Cc/Bcc" }).click();
  await form.getByLabel("Bcc", { exact: true }).fill("archive@example.test");
  await form.getByLabel("Subject", { exact: true }).fill("Quarterly receipts");
  await form.getByLabel("Message", { exact: true }).fill("Here are the receipts.");
  await expect(form.getByRole("status")).toHaveText("Draft saved on this device");
  await expect(page).toHaveURL(/draft=draft-/);
  await expect(page.getByRole("link", { name: /Drafts/ })).toContainText("1");
  await expect(form.getByLabel("Signature preview")).toContainText("Sent with care");
  await page.screenshot({ path: testInfo.outputPath("composer.png") });

  await page.reload();
  const restored = page.getByRole("form", { name: "New message" });
  await expect(restored.getByLabel("Message", { exact: true })).toHaveValue(
    "Here are the receipts.",
  );
  await expect(restored.getByLabel("Bcc", { exact: true })).toHaveValue(
    "archive@example.test",
  );

  await restored.getByRole("button", { name: "Send", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Outbox", exact: true }),
  ).toBeVisible();
  const undoBar = page.locator(".undo-send-bar");
  await expect(undoBar).toContainText("Sending Quarterly receipts in");
  await expect(page.getByText(/Sending in \d+s/)).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath("undo-send.png") });
  await undoBar.getByRole("button", { name: "Undo" }).click();
  await expect(page).toHaveURL(/compose\?draft=/);
  await expect(
    page.getByLabel("Message", { exact: true }),
  ).toHaveValue("Here are the receipts.");
  await expect(page.locator(".undo-send-bar")).toHaveCount(0);
});

test("an unknown outcome is explained and only resent after confirmation", async ({
  page,
}, testInfo) => {
  await fixture(page, { undo: 0, outcome: "unknown" });
  await page.goto("/compose");
  const form = page.getByRole("form", { name: "New message" });
  await form.getByRole("button", { name: "Send", exact: true }).click();
  await expect(form.getByRole("alert")).toContainText("add at least one recipient");
  await form.getByLabel("To", { exact: true }).fill("bob@example.test");
  await form.getByLabel("Subject", { exact: true }).fill("Contract");
  await form.getByRole("button", { name: "Send", exact: true }).click();

  const item = page.getByRole("listitem").filter({ hasText: "Contract" });
  await expect(item).toContainText("can’t tell whether this was sent");
  await expect(item).toContainText("won’t be sent again unless you choose to");
  await expect(page.getByRole("link", { name: /Outbox/ })).toContainText("1");
  // The refused first attempt and the sent message were the same draft.
  await expect(page.getByRole("link", { name: /Drafts/ })).not.toContainText(/\d/);
  await page.screenshot({ path: testInfo.outputPath("unknown-outcome.png") });

  await item.getByRole("button", { name: "Send again…" }).click();
  const confirm = page.getByRole("dialog", { name: "Send again" });
  await expect(confirm).toContainText("recipients could get it twice");
  await confirm.getByRole("button", { name: "Cancel" }).click();
  await expect(item).toContainText("can’t tell whether this was sent");

  await item.getByRole("button", { name: "It was sent" }).click();
  await expect(item).toHaveCount(0);
});

test("an account without outgoing mail settings can add them from the composer", async ({
  page,
}) => {
  await fixture(page, { smtp: false });
  await page.goto("/compose");
  const form = page.getByRole("form", { name: "New message" });
  await expect(
    form.getByRole("button", { name: "Send", exact: true }),
  ).toBeDisabled();
  await form.getByRole("button", { name: "Add outgoing mail" }).click();
  const dialog = page.getByRole("dialog", { name: "Manage account" });
  await expect(dialog.getByLabel("Outgoing (SMTP) server")).toHaveValue(
    "smtp.example.test",
  );
  await expect(dialog.getByLabel("Signature", { exact: true })).toHaveCount(0);
  await dialog
    .getByRole("button", { name: "Save sending settings" })
    .click();
  await expect(dialog.getByRole("status")).toContainText("Sending settings saved");
  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(
    form.getByRole("button", { name: "Send", exact: true }),
  ).toBeEnabled();
});

test("the undo delay is a setting", async ({ page }) => {
  await fixture(page, {});
  await page.goto("/settings");
  const group = page.getByRole("group", { name: "Undo send" });
  await expect(group.getByRole("button", { name: "10s" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await group.getByRole("button", { name: "30s" }).click();
  await expect(group.getByRole("button", { name: "30s" })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await page.reload();
  await expect(
    page.getByRole("group", { name: "Undo send" }).getByRole("button", { name: "30s" }),
  ).toHaveAttribute("aria-pressed", "true");
});

async function writeAndSend(page: Page) {
  await page.goto("/compose");
  const form = page.getByRole("form", { name: "New message" });
  await form.getByLabel("To", { exact: true }).fill("bob@example.test");
  await form.getByLabel("Subject", { exact: true }).fill("Budget");
  await form.getByLabel("Message", { exact: true }).fill("Numbers attached.");
  await form.getByRole("button", { name: "Send", exact: true }).click();
  await expect(
    page.getByRole("heading", { name: "Outbox", exact: true }),
  ).toBeVisible();
  return page.getByRole("listitem").filter({ hasText: "Budget" });
}

async function expectReopened(page: Page) {
  await expect(page).toHaveURL(/compose\?draft=/);
  const form = page.getByRole("form", { name: "New message" });
  await expect(form.getByLabel("To", { exact: true })).toHaveValue(
    "bob@example.test",
  );
  await expect(form.getByLabel("Message", { exact: true })).toHaveValue(
    "Numbers attached.",
  );
}

test("undo from an outbox row reopens the message with its text", async ({
  page,
}) => {
  await fixture(page, { undo: 30 });
  const item = await writeAndSend(page);
  await item.getByRole("button", { name: "Undo", exact: true }).click();
  await expectReopened(page);
});

test("editing a failed message reopens it with its text", async ({
  page,
}, testInfo) => {
  await fixture(page, { undo: 0, outcome: "failed" });
  const item = await writeAndSend(page);
  await expect(item).toContainText("Not sent: the server refused");
  await page.screenshot({ path: testInfo.outputPath("failed-send.png") });
  await item.getByRole("button", { name: "Edit", exact: true }).click();
  await expectReopened(page);
});

test("an undo that is too late says so and leaves the message in the outbox", async ({
  page,
}) => {
  await fixture(page, { undo: 30 });
  const item = await writeAndSend(page);
  await page.evaluate(() => localStorage.setItem("pigeon-test-too-late", "1"));
  await item.getByRole("button", { name: "Undo", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText("can no longer be undone");
  await expect(page).toHaveURL(/\/outbox/);
  await expect(item).toBeVisible();
});

test("a new message opened from a draft starts empty and leaves the draft intact", async ({
  page,
}) => {
  await fixture(page, {
    drafts: [{ to: "carol@example.test", subject: "Kept plans", body: "Original text" }],
  });
  await page.goto("/drafts");
  await page.getByRole("link", { name: /Kept plans/ }).click();
  const form = page.getByRole("form", { name: "New message" });
  await expect(form.getByLabel("Message", { exact: true })).toHaveValue(
    "Original text",
  );

  await page.getByRole("link", { name: "New message", exact: true }).click();
  await expect(page).not.toHaveURL(/draft=/);
  await expect(form.getByLabel("To", { exact: true })).toHaveValue("");
  await expect(form.getByLabel("Subject", { exact: true })).toHaveValue("");
  await expect(form.getByLabel("Message", { exact: true })).toHaveValue("");
  await form.getByLabel("Subject", { exact: true }).fill("Fresh idea");
  await expect(form.getByRole("status")).toHaveText("Draft saved on this device");

  await page.goto("/drafts");
  await expect(page.getByRole("link", { name: /Fresh idea/ })).toBeVisible();
  await page.getByRole("link", { name: /Kept plans/ }).click();
  await expect(form.getByLabel("Message", { exact: true })).toHaveValue(
    "Original text",
  );
  await expect(form.getByLabel("To", { exact: true })).toHaveValue(
    "carol@example.test",
  );
});



test("restored outbox work stays paused until an explicit reviewed decision", async ({ page }, testInfo) => {
  await fixture(page, { undo: 0, outcome: "held" });
  await page.goto("/compose");
  const form = page.getByRole("form", { name: "New message" });
  await form.getByLabel("To", { exact: true }).fill("bob@example.test");
  await form.getByLabel("Subject", { exact: true }).fill("Restored canary");
  await form.getByRole("button", { name: "Send", exact: true }).click();
  const item = page.getByRole("listitem").filter({ hasText: "Restored canary" });
  await expect(item).toContainText("Paused until you review it");
  await item.getByRole("button", { name: "Send again…" }).click();
  const confirm = page.getByRole("dialog", { name: "Send again" });
  await expect(confirm).toContainText("recipients could get it twice");
  await confirm.getByRole("button", { name: "Cancel" }).click();
  await expect(item).toContainText("Paused until you review it");
  await page.screenshot({ path: testInfo.outputPath("restored-outbox-held.png") });
  await item.getByRole("button", { name: "It was sent" }).click();
  await expect(item).toHaveCount(0);
});


test("confirmed recipients are suggested by name and email across reloads and copy fields", async ({ page }, testInfo) => {
  await fixture(page, { undo: 0 });
  await page.goto("/compose");
  const to = page.getByLabel("To", { exact: true });
  await to.fill('"Scudder, Sam" <sam@example.test>');
  await page.getByRole("button", { name: "Cc/Bcc" }).click();
  await page.getByLabel("Cc", { exact: true }).fill("Alex <alex@example.test>");
  await page.getByLabel("Bcc", { exact: true }).fill("archive@example.test");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await expect(page.getByText("Nothing waiting to send.")).toBeVisible();
  await page.getByRole("link", { name: "New message", exact: true }).click();
  await page.reload();
  await to.fill("SCUD");
  const sam = page.getByRole("option", { name: "Scudder, Sam sam@example.test" });
  await expect(sam).toBeVisible();
  await page.screenshot({ path: testInfo.outputPath("recipient-suggestions.png") });
  await to.press("Enter");
  await expect(to).toHaveValue('"Scudder, Sam" <sam@example.test>, ');
  await expect(page).toHaveURL(/compose/);
  await to.fill('"Scudder, Sam" <sam@example.test>, SAM@');
  await expect(page.getByRole("listbox")).toHaveCount(0);
  await to.fill('"Scudder, Sam" <sam@example.test>, alex@');
  await page.getByRole("option", { name: "Alex alex@example.test" }).click();
  await expect(to).toHaveValue('"Scudder, Sam" <sam@example.test>, "Alex" <alex@example.test>, ');
  await page.getByRole("button", { name: "Cc/Bcc" }).click();
  const cc = page.getByLabel("Cc", { exact: true });
  await cc.fill("scud");
  await cc.press("Escape");
  await expect(page.getByRole("listbox")).toHaveCount(0);
  await expect(cc).toHaveValue("scud");
  await cc.fill("sam@");
  await cc.press("ArrowDown");
  await cc.press("Tab");
  await expect(cc).toHaveValue('"Scudder, Sam" <sam@example.test>, ');
  const bcc = page.getByLabel("Bcc", { exact: true });
  await bcc.fill("ARCHIVE@");
  await page.getByRole("option", { name: "archive@example.test" }).click();
  await expect(bcc).toHaveValue("archive@example.test, ");
  await to.fill("nobody-matches");
  await expect(page.getByRole("listbox")).toHaveCount(0);
});

for (const outcome of ["failed", "unknown"] as const) {
  test(`${outcome} sends do not teach recipient suggestions`, async ({ page }) => {
    await fixture(page, { undo: 0, outcome });
    await writeAndSend(page);
    await page.getByRole("link", { name: "New message", exact: true }).click();
    await page.getByLabel("To", { exact: true }).fill("bob");
    await expect(page.getByRole("listbox")).toHaveCount(0);
  });
}

test("signatures have their own settings page and persist into the composer", async ({ page }, testInfo) => {
  await fixture(page, {});
  await page.goto("/settings");
  await page.getByRole("link", { name: "Manage signatures" }).click();
  await expect(page).toHaveURL(/settings\/signatures$/);
  const form = page.getByRole("form", { name: "Personal" });
  await expect(form.getByLabel("Signature", { exact: true })).toHaveValue("Sent with care");
  await form.getByLabel("Signature", { exact: true }).fill("Kind regards,\nTest Person");
  await form.getByRole("button", { name: "Save signature" }).click();
  await expect(form.getByRole("status")).toHaveText("Signature saved.");
  await page.reload();
  await expect(form.getByLabel("Signature", { exact: true })).toHaveValue("Kind regards,\nTest Person");
  await page.screenshot({ path: testInfo.outputPath("signatures.png"), fullPage: true });
  await page.goto("/compose");
  await expect(page.getByLabel("Signature preview", { exact: true })).toHaveText("-- \nKind regards,\nTest Person");
  await expect(page.getByLabel("From")).toContainText("Test Person");
  await expect(page.getByRole("button", { name: "Send", exact: true })).toBeEnabled();
  await page.goto("/settings/signatures");
  await form.getByLabel("Signature", { exact: true }).fill("");
  await form.getByRole("button", { name: "Save signature" }).click();
  await expect(form.getByRole("status")).toHaveText("Signature saved.");
  await page.reload();
  await expect(form.getByLabel("Signature", { exact: true })).toHaveValue("");
  await page.goto("/compose");
  await expect(page.getByRole("heading", { name: "New message", exact: true })).toBeVisible();
  await expect(page.getByLabel("Signature preview", { exact: true })).toHaveCount(0);
});

test("a signature can be saved before outgoing mail is configured", async ({ page }) => {
  await fixture(page, { smtp: false });
  await page.goto("/settings/signatures");
  await page.getByLabel("Signature", { exact: true }).fill("Thanks!");
  await page.getByRole("button", { name: "Save signature" }).click();
  await expect(page.getByRole("status")).toContainText("Signature saved.");
  await page.reload();
  await expect(page.getByLabel("Signature", { exact: true })).toHaveValue("Thanks!");
  await page.goto("/compose");
  await page.getByRole("button", { name: "Add outgoing mail" }).click();
  const dialog = page.getByRole("dialog", { name: "Manage account" });
  await dialog.getByRole("button", { name: "Save sending settings" }).click();
  await expect(dialog.getByRole("status")).toContainText("Sending settings saved");
  await dialog.getByRole("link", { name: "Signatures settings" }).click();
  await expect(page).toHaveURL(/settings\/signatures$/);
  await expect(dialog).toHaveCount(0);
  await expect(page.getByRole("textbox", { name: "Signature", exact: true })).toHaveValue("Thanks!");
});

test("signature save failures keep the edit without claiming it was saved", async ({ page }) => {
  await fixture(page, { failSendingSave: true });
  await page.goto("/settings/signatures");
  await page.getByLabel("Signature", { exact: true }).fill("Unsaved edit");
  await page.getByRole("button", { name: "Save signature" }).click();
  await expect(page.getByRole("alert")).toContainText("Could not save signature");
  await expect(page.getByLabel("Signature", { exact: true })).toHaveValue("Unsaved edit");
  await expect(page.getByText("Signature saved.", { exact: true })).toHaveCount(0);
  await page.reload();
  await expect(page.getByLabel("Signature", { exact: true })).toHaveValue("Sent with care");
});

test("managed account signatures are read-only", async ({ page }) => {
  await fixture(page, { managed: true });
  await page.goto("/settings/signatures");
  await expect(page.getByLabel("Signature", { exact: true })).toHaveValue("Sent with care");
  await expect(page.getByLabel("Signature", { exact: true })).toBeDisabled();
  await expect(page.getByText("Managed by Home Manager.", { exact: false })).toBeVisible();
  await expect(page.getByRole("button", { name: "Save signature" })).toHaveCount(0);
});


test("multiple signatures can be named, saved, selected per draft and removed", async ({ page }, testInfo) => {
  await fixture(page, {});
  await page.goto("/settings");
  await expect(page.getByRole("link", { name: "Manage signatures" })).toBeVisible();
  await page.getByRole("link", { name: "Manage signatures" }).scrollIntoViewIfNeeded();
  await page.screenshot({ path: testInfo.outputPath("01-settings-navigation.png"), fullPage: true });
  await page.getByRole("link", { name: "Manage signatures" }).click();
  const form = page.getByRole("form", { name: "Personal" });
  await form.getByRole("button", { name: "Add signature" }).click();
  await form.getByLabel("Signature name", { exact: true }).fill("Work");
  await form.getByLabel("Signature", { exact: true }).fill("Test Person | Example Team");
  await form.getByLabel("Default signature", { exact: true }).selectOption({ label: "Work" });
  await form.getByRole("button", { name: "Save signature", exact: true }).click();
  await expect(form.getByRole("status")).toHaveText("Signature saved.");
  await page.reload();
  await expect(form.getByLabel("Saved signature")).toContainText("Work");
  await expect(form.getByLabel("Default signature").locator("option:checked")).toHaveText("Work");
  await form.getByLabel("Saved signature").selectOption({ label: "Work" });
  await expect(form.getByLabel("Signature", { exact: true })).toHaveValue("Test Person | Example Team");
  await page.screenshot({ path: testInfo.outputPath("02-multiple-signatures.png"), fullPage: true });
  await page.goto("/compose");
  await expect(page.getByLabel("Signature preview")).toContainText("Test Person | Example Team");
  await page.getByRole("combobox", { name: "Signature", exact: true }).selectOption({ label: "Work" });
  await page.getByLabel("Message", { exact: true }).fill("Hello,\n\nHere is the update we discussed.");
  await expect(page.getByRole("status")).toContainText("Draft saved on this device");
  await expect(page.getByRole("combobox", { name: "Signature", exact: true }).locator("option:checked")).toHaveText("Work");
  await expect(page.getByLabel("Signature preview")).toContainText("Test Person | Example Team");
  await page.getByLabel("Signature preview").scrollIntoViewIfNeeded();
  await page.screenshot({ path: testInfo.outputPath("03-composer-signature-applied.png"), fullPage: true });
  await page.getByRole("combobox", { name: "Signature", exact: true }).selectOption({ label: "Default" });
  await page.getByLabel("Subject", { exact: true }).fill("Signature choice");
  await expect(page.getByRole("status")).toContainText("Draft saved on this device");
  await page.reload();
  await expect(page.getByLabel("Signature preview")).toContainText("Sent with care");
  await page.getByRole("combobox", { name: "Signature", exact: true }).selectOption({ label: "No signature" });
  await expect(page.getByRole("status")).toContainText("Draft saved on this device");
  await page.reload();
  await expect(page.getByRole("combobox", { name: "Signature", exact: true })).toHaveValue("none");
  await expect(page.getByLabel("Signature preview")).toHaveCount(0);
  await page.goto("/settings/signatures");
  await form.getByLabel("Saved signature").selectOption({ label: "Work" });
  await form.getByRole("button", { name: "Remove signature" }).click();
  await form.getByRole("button", { name: "Save signature", exact: true }).click();
  await expect(form.getByRole("status")).toHaveText("Signature saved.");
  await page.reload();
  await expect(form.getByLabel("Saved signature").getByRole("option", { name: "Work", exact: true })).toHaveCount(0);
  await expect(form.getByLabel("Default signature")).toHaveValue("");
});
