import { type QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createRootRouteWithContext,
  ClientOnly,
  HeadContent,
  Outlet,
  Scripts,
} from "@tanstack/react-router";
import type { ReactNode } from "react";
import { Workspace } from "../components/workspace";
import { MailProvider } from "../components/mail-provider";
import css from "../styles.css?url";

export const Route = createRootRouteWithContext<{ queryClient: QueryClient }>()(
  {
    head: () => ({
      meta: [
        { charSet: "utf-8" },
        { name: "viewport", content: "width=device-width, initial-scale=1" },
        { title: "Pigeon" },
      ],
      links: [{ rel: "stylesheet", href: css }],
    }),
    shellComponent: RootDocument,
    component: RootLayout,
  },
);

function RootDocument({ children }: { children: ReactNode }) {
  return (
    <html lang="en">
      <head>
        <HeadContent />
      </head>
      <body>
        {children}
        <Scripts />
      </body>
    </html>
  );
}

function RootLayout() {
  const { queryClient } = Route.useRouteContext();
  return (
    <QueryClientProvider client={queryClient}>
      <ClientOnly
        fallback={
          <div className="launch-screen" role="status">
            Opening Pigeon…
          </div>
        }
      >
        <MailProvider><Workspace><Outlet /></Workspace></MailProvider>
      </ClientOnly>
    </QueryClientProvider>
  );
}
