import { expect, test, type Page } from "@playwright/test";

// Native IPC fixture for the backup screens. Encryption, file handling and
// restore safety are exercised by crates/pigeon-core/tests/backup.rs.
async function desktop(page: Page, options: { keyMissing?: boolean } = {}) {
  await page.addInitScript((keyMissing) => {
    const calls: { command: string; args: Record<string, unknown> }[] = [];
    (window as any).__calls = calls;
    const account = {
      id: "acc-1",
      config: {
        name: "Synthetic",
        address: "mail@example.test",
        host: "imap.example.test",
        port: 993,
        username: "mail@example.test",
        security: "tls",
        ca_certificate: null,
      },
      status: "idle",
      error: null,
      last_synced: null,
      sync_folder: null,
      sync_cached: 0,
      sync_total: 0,
    };
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      value: {
        invoke: async (command: string, args: Record<string, any> = {}) => {
          calls.push({ command, args });
          switch (command) {
            case "mail_state":
              if (keyMissing)
                throw {
                  code: "key_missing",
                  message: "this device's mail store key is missing",
                };
              return { accounts: [account], folders: [], saved_searches: [] };
            case "backup_mail":
              return {
                accounts: 1,
                passwords: 1,
                saved_searches: 2,
                previous_store: null,
              };
            case "restore_mail":
              if (args.passphrase !== "correct horse battery staple")
                throw {
                  code: "wrong_passphrase",
                  message:
                    "the passphrase does not open this backup, or the backup has been altered",
                };
              return {
                accounts: 1,
                passwords: 1,
                saved_searches: 2,
                previous_store: "mail.db.before-restore-1793005200",
              };
            case "cancel_mail_sync":
              return null;
            default:
              throw { code: "unsupported", message: command };
          }
        },
      },
    });
  }, options.keyMissing ?? false);
}

const calls = (page: Page, command: string) =>
  page.evaluate(
    (command) =>
      (window as any).__calls.filter((c: any) => c.command === command),
    command,
  );

test("settings explain the loss model outside the desktop app", async ({
  page,
}) => {
  await page.goto("/settings");
  const section = page.locator("#backup");
  await expect(
    section.getByRole("heading", { name: "Backup and restore" }),
  ).toBeVisible();
  const facts = section.getByRole("list", { name: "What you can lose" });
  await expect(facts).toContainText("Mail on your servers is unaffected");
  await expect(facts).toContainText("gone for good");
  await expect(facts).toContainText("Restoring never sends anything");
  await expect(section).toContainText("available in Pigeon’s desktop app");
  await expect(
    section.getByRole("button", { name: "Back up now…" }),
  ).toHaveCount(0);
});

test("backup checks the passphrase before asking where to save", async ({
  page,
}) => {
  await desktop(page);
  await page.goto("/settings");
  await page.getByRole("button", { name: "Back up now…" }).click();
  const dialog = page.getByRole("dialog", { name: "Back up local data" });
  const save = dialog.getByRole("button", { name: "Choose where to save…" });

  await dialog.getByLabel("Backup passphrase").fill("short");
  await dialog.getByLabel("Type it again").fill("short");
  await save.click();
  await expect(dialog.getByRole("alert")).toHaveText(
    "Use a passphrase of at least 12 characters.",
  );

  await dialog.getByLabel("Backup passphrase").fill("correct horse battery");
  await dialog.getByLabel("Type it again").fill("correct horse batery");
  await save.click();
  await expect(dialog.getByRole("alert")).toHaveText(
    "The passphrases do not match.",
  );
  expect(await calls(page, "backup_mail")).toHaveLength(0);

  await dialog.getByLabel("Type it again").fill("correct horse battery");
  await save.click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("status")).toHaveText(
    "Backup saved with 1 account, 1 password, 2 saved searches.",
  );
  expect(await calls(page, "backup_mail")).toEqual([
    { command: "backup_mail", args: { passphrase: "correct horse battery" } },
  ]);
});

test("restore reports a wrong passphrase, then stops syncing and restores", async ({
  page,
}) => {
  await desktop(page);
  await page.goto("/settings");
  await page.getByRole("button", { name: "Restore from backup…" }).click();
  const dialog = page.getByRole("dialog", { name: "Restore from backup" });
  await expect(dialog).toContainText("kept aside, not deleted");
  await dialog.getByLabel("The backup’s passphrase").fill("not the passphrase");
  await dialog.getByRole("button", { name: "Choose backup file…" }).click();
  await expect(dialog.getByRole("alert")).toContainText(
    "the passphrase does not open this backup",
  );

  await dialog
    .getByLabel("The backup’s passphrase")
    .fill("correct horse battery staple");
  await dialog.getByRole("button", { name: "Choose backup file…" }).click();
  await expect(dialog).toBeHidden();
  await expect(page.getByRole("status")).toContainText(
    "Restored 1 account, 1 password, 2 saved searches.",
  );
  await expect(page.getByRole("status")).toContainText(
    "mail.db.before-restore-1793005200",
  );
  expect(await calls(page, "cancel_mail_sync")).toEqual([
    { command: "cancel_mail_sync", args: { id: "acc-1" } },
    { command: "cancel_mail_sync", args: { id: "acc-1" } },
  ]);
});

test("a missing store key points to restore instead of unlock", async ({
  page,
}) => {
  await desktop(page, { keyMissing: true });
  await page.goto("/mail");
  await expect(
    page.getByRole("heading", { name: "This device’s mail key is missing" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "Unlock store" })).toHaveCount(
    0,
  );
  await page.getByRole("link", { name: "Restore from a backup" }).click();
  await expect(page).toHaveURL(/\/settings#backup$/);
  await expect(
    page.getByRole("button", { name: "Restore from backup…" }),
  ).toBeVisible();
});
