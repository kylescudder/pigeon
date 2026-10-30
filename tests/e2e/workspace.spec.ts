import { expect, test } from "@playwright/test";

test("first run explains the preview and opens real diagnostics", async ({
  page,
}) => {
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "Your inbox starts here." }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Connect an account", exact: true }),
  ).toBeDisabled();
  await page.getByRole("button", { name: "About Pigeon", exact: true }).click();
  const dialog = page.getByRole("dialog", {
    name: "A better home for your mail",
  });
  await expect(dialog).toBeVisible();
  await expect(
    dialog.getByText(/This build receives and searches mail/),
  ).toBeVisible();
  await dialog.getByRole("button", { name: "Open diagnostics" }).click();
  await expect(dialog).not.toBeVisible();
  await expect(page).toHaveURL(/\/foundation$/);
  await expect(
    page.getByRole("heading", { name: "Diagnostics", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByText("Native checks need the desktop app"),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Re-run checks" }),
  ).toBeDisabled();
  await expect(
    page.getByRole("button", { name: "Open isolation probe" }),
  ).toBeDisabled();
  await expect(
    page.getByRole("button", { name: "Open sanitised fixture" }),
  ).toBeDisabled();
  await expect(page.getByText("Passed", { exact: true })).toHaveCount(0);
});

test("command menu searches, handles empty results, navigates, and restores focus", async ({
  page,
}) => {
  await page.goto("/");
  const trigger = page.getByRole("button", {
    name: "Open command menu",
    exact: true,
  });
  await trigger.click();
  const dialog = page.getByRole("dialog", { name: "Command menu" });
  const search = dialog.getByRole("combobox", { name: "Search commands" });
  await expect(search).toBeFocused();
  await search.fill("nothing matches this");
  await expect(dialog.getByText(/No commands found/)).toBeVisible();
  await search.press("Enter");
  await expect(dialog).toBeVisible();
  await search.press("Escape");
  await expect(dialog).not.toBeVisible();
  await expect(trigger).toBeFocused();
  await page.keyboard.press("Control+k");
  await expect(dialog).toBeVisible();
  await expect(search).toHaveValue("");
  await search.fill("diagnostics");
  await expect(dialog.getByRole("option")).toHaveCount(1);
  await search.press("ArrowDown");
  await search.press("Enter");
  await expect(page).toHaveURL(/\/foundation$/);
  await expect(dialog).not.toBeVisible();
  await page.reload();
  await expect(
    page.getByRole("heading", { name: "Diagnostics", exact: true }),
  ).toBeVisible();
});

test("appearance and effective density survive navigation and reload", async ({
  page,
}) => {
  await page.goto("/");
  const originalHeight = await page
    .getByRole("link", { name: /Make it yours/ })
    .evaluate((element) => element.getBoundingClientRect().height);
  await page.getByRole("link", { name: /Make it yours/ }).click();
  await page.getByRole("button", { name: "Dark", exact: true }).click();
  await page.getByRole("button", { name: "Compact", exact: true }).click();
  await page.reload();
  await expect(
    page.getByRole("button", { name: "Dark", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect(
    page.getByRole("button", { name: "Compact", exact: true }),
  ).toHaveAttribute("aria-pressed", "true");
  await expect
    .poll(() =>
      page
        .locator("html")
        .evaluate((element) => getComputedStyle(element).colorScheme),
    )
    .toBe("dark");
  await page.getByRole("link", { name: "Pigeon home" }).click();
  const compactHeight = await page
    .getByRole("link", { name: /Make it yours/ })
    .evaluate((element) => element.getBoundingClientRect().height);
  expect(compactHeight).toBeLessThan(originalHeight);
  await page.getByRole("link", { name: /Make it yours/ }).click();
  await page.getByRole("button", { name: "Light", exact: true }).click();
  await expect
    .poll(() =>
      page
        .locator("html")
        .evaluate((element) => getComputedStyle(element).colorScheme),
    )
    .toBe("light");
  await page.getByRole("button", { name: "System", exact: true }).click();
  await page.emulateMedia({ colorScheme: "dark" });
  await expect
    .poll(() =>
      page
        .locator("html")
        .evaluate((element) => getComputedStyle(element).colorScheme),
    )
    .toBe("dark");
  await page.emulateMedia({ colorScheme: "light" });
  await expect
    .poll(() =>
      page
        .locator("html")
        .evaluate((element) => getComputedStyle(element).colorScheme),
    )
    .toBe("light");
});

test("real pages render without overflow or browser errors", async ({
  page,
}, testInfo) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  page.on("console", (message) => {
    if (message.type() === "error") errors.push(message.text());
  });
  for (const [path, heading] of [
    ["/", "Overview"],
    ["/settings", "Appearance"],
    ["/foundation", "Diagnostics"],
  ]) {
    await page.goto(path!);
    await expect(
      page.getByRole("heading", { name: heading, exact: true }),
    ).toBeVisible();
    await expect(
      page.getByRole("link", { name: heading, exact: true }),
    ).toHaveAttribute("aria-current", "page");
    await expect(page.locator(".nav-item[aria-current='page']")).toHaveCount(1);
    await expect
      .poll(() =>
        page.evaluate(
          () =>
            document.documentElement.scrollWidth <=
            document.documentElement.clientWidth,
        ),
      )
      .toBe(true);
    await expect
      .poll(() =>
        page
          .locator(".page-area")
          .evaluate((element) => element.scrollWidth <= element.clientWidth),
      )
      .toBe(true);
    await page.screenshot({
      path: testInfo.outputPath(`${heading?.toLowerCase()}.png`),
    });
  }
  expect(errors).toEqual([]);
});

// Only frontend rendering/recovery is tested here. This IPC stub deliberately
// does not assert real Secret Service behaviour or native command isolation.
test("native results, retry, and viewer failures remain visible and actionable", async ({
  page,
}) => {
  await page.addInitScript(() => {
    let checks = 0;
    let fixtureOpens = 0;
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      value: {
        invoke: async (command: string) => {
          if (command === "mail_state") return { accounts: [], folders: [], saved_searches: [] };
          if (command === "foundation_report") {
            checks++;
            if (checks === 1)
              throw new Error("Test device temporarily unavailable");
            return {
              checks: [
                {
                  name: "Keyring",
                  ok: true,
                  detail: "unlocked (test fixture)",
                },
                {
                  name: "Viewer native-command denial",
                  ok: checks > 2,
                  detail:
                    checks > 2
                      ? "probe completed (test fixture)"
                      : "probe not run yet (test fixture)",
                },
              ],
            };
          }
          if (command === "open_viewer_fixture" && ++fixtureOpens === 1)
            throw new Error("Test viewer failed to open");
          if (command === "quit_app")
            throw new Error("Quit blocked in frontend test");
          if (["open_viewer_fixture", "open_viewer_probe"].includes(command))
            return;
          throw new Error(`Unexpected test command: ${command}`);
        },
      },
    });
  });
  await page.goto("/foundation");
  await expect(page.getByRole("alert")).toContainText(
    "Test device temporarily unavailable",
  );
  await page.getByRole("button", { name: "Re-run checks" }).click();
  await expect(page.getByRole("alert")).toHaveCount(0);
  await expect(page.getByText("1 of 2 passed")).toBeVisible();
  await expect(
    page.getByText("Needs attention", { exact: true }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Open sanitised fixture" }).click();
  await expect(page.getByRole("alert")).toContainText(
    "Test viewer failed to open",
  );
  await page.getByRole("button", { name: "Open sanitised fixture" }).click();
  await expect(page.getByRole("alert")).toHaveCount(0);
  await expect(page.getByRole("status")).toContainText(
    "Viewer opened in a separate window",
  );
  await page.getByRole("button", { name: "Open isolation probe" }).click();
  await page.getByRole("button", { name: "Re-run checks" }).click();
  await expect(page.getByText("2 of 2 passed")).toBeVisible();
  await expect(page.getByText("Needs attention", { exact: true })).toHaveCount(
    0,
  );
  await page.getByRole("button", { name: "Quit Pigeon", exact: true }).click();
  await expect(page.getByRole("alert")).toContainText(
    "Quit blocked in frontend test",
  );
});

test("icon-only buttons centre their icons", async ({ page }) => {
  await page.goto("/");
  const buttons = page.locator("button:has(> svg:only-child)");
  await expect(page.locator(".workspace-avatar")).toBeVisible();
  const offsets = await buttons.evaluateAll((elements) =>
    elements
      .filter((element) => {
        const box = element.getBoundingClientRect();
        return box.width > 0 && box.width <= 48 && !element.textContent?.trim();
      })
      .map((element) => {
        const box = element.getBoundingClientRect();
        const icon = element.querySelector("svg")!.getBoundingClientRect();
        return {
          label: element.getAttribute("aria-label"),
          x: icon.left + icon.width / 2 - (box.left + box.width / 2),
          y: icon.top + icon.height / 2 - (box.top + box.height / 2),
        };
      }),
  );
  expect(offsets.map((offset) => offset.label)).toContain(
    "About this local workspace",
  );
  for (const offset of offsets) {
    expect(Math.abs(offset.x), `${offset.label} x`).toBeLessThanOrEqual(0.5);
    expect(Math.abs(offset.y), `${offset.label} y`).toBeLessThanOrEqual(0.5);
  }
});
