import { type QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  createRootRouteWithContext,
  HeadContent,
  Link,
  Outlet,
  Scripts,
} from "@tanstack/react-router";
import type { ReactNode } from "react";
import { isDesktop, quitApp } from "../lib/native";
import css from "../styles.css?url";

export const Route = createRootRouteWithContext<{ queryClient: QueryClient }>()({
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
});

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
      <div className="app">
        <nav className="sidebar">
          <strong className="brand">Pigeon</strong>
          <Link to="/" activeOptions={{ exact: true }}>
            Overview
          </Link>
          <Link to="/foundation">Foundation checks</Link>
          {isDesktop() && (
            <button type="button" className="quit" onClick={() => quitApp()}>
              Quit Pigeon
            </button>
          )}
        </nav>
        <main className="content">
          <Outlet />
        </main>
      </div>
    </QueryClientProvider>
  );
}
