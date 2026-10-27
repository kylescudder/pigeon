// Route values contain opaque local identifiers, never search terms, mail
// addresses, or folder names. Unsaved search text is intentionally session-only.
export type MailRouteSearch = {
  account?: string;
  folder?: string;
  view?: "inbox" | "all";
  saved?: string;
};

export function validateMailRoute(
  search: Record<string, unknown>,
): MailRouteSearch {
  return {
    account: typeof search.account === "string" ? search.account : undefined,
    folder: typeof search.folder === "string" ? search.folder : undefined,
    view:
      search.view === "inbox" || search.view === "all"
        ? search.view
        : undefined,
    saved: typeof search.saved === "string" ? search.saved : undefined,
  };
}
