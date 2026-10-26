import { createFileRoute } from "@tanstack/react-router";

export const Route = createFileRoute("/")({ component: Overview });

function Overview() {
  return (
    <section>
      <h1>Overview</h1>
      <p>
        No accounts yet. Milestone 0 proves the shell, encrypted store, keyring, and
        message-viewer boundary before mail sync is added.
      </p>
    </section>
  );
}
