import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { Icon } from "../components/icon";
import { PageHeading } from "../components/workspace";
import {
  foundationReport,
  isDesktop,
  openViewerFixture,
  openViewerProbe,
} from "../lib/native";

export const Route = createFileRoute("/foundation")({ component: Foundation });

function Foundation() {
  const [desktop, setDesktop] = useState(false);
  useEffect(() => setDesktop(isDesktop()), []);
  const report = useQuery({
    queryKey: ["foundation-report"],
    queryFn: foundationReport,
    enabled: desktop,
  });
  const viewer = useMutation({ mutationFn: openViewerFixture });
  const probe = useMutation({ mutationFn: openViewerProbe });

  const passed = report.data?.checks.filter((check) => check.ok).length ?? 0;

  return (
    <section className="page diagnostics-page">
      <PageHeading
        title="Diagnostics"
        description="A closer look at your local workspace."
      >
        <button
          type="button"
          className="button button-secondary"
          disabled={!desktop || report.isFetching}
          onClick={() => {
            void report.refetch();
          }}
        >
          <Icon
            name="refresh"
            size={15}
            className={report.isFetching ? "spinning" : undefined}
          />
          {report.isFetching ? "Checking…" : "Re-run checks"}
        </button>
      </PageHeading>
      {!desktop && (
        <div className="notice">
          <Icon name="info" size={19} />
          <div>
            <strong>Native checks need the desktop app</strong>
            <p>
              You’re viewing Pigeon in a browser. Open the desktop app to check
              its keyring, encrypted storage, and message viewer.
            </p>
          </div>
        </div>
      )}
      {report.isError && (
        <div className="notice notice-error" role="alert">
          <Icon name="alert" size={19} />
          <div>
            <strong>Couldn’t complete the device checks</strong>
            <p>{String(report.error)}</p>
            <p>Check that your desktop keyring is available, then try again.</p>
          </div>
        </div>
      )}
      <section className="checks-section" aria-labelledby="device-checks">
        <div className="section-heading">
          <h2 id="device-checks">Foundation checks</h2>
          <span>
            {report.data
              ? `${passed} of ${report.data.checks.length} passed`
              : desktop
                ? "This device"
                : "Desktop only"}
          </span>
        </div>
        <div
          className="check-list"
          aria-busy={desktop && report.isFetching}
          aria-live="polite"
        >
          {report.data
            ? report.data.checks.map((check) => (
                <div className="check-row" key={check.name}>
                  <span
                    className={`check-icon ${check.ok ? "status-ok" : "status-bad"}`}
                  >
                    <Icon name={check.ok ? "checkCircle" : "alert"} size={19} />
                  </span>
                  <div className="check-copy">
                    <strong>{check.name}</strong>
                    <p>{check.detail}</p>
                  </div>
                  <span
                    className={`status-pill ${check.ok ? "status-ok" : "status-bad"}`}
                  >
                    {check.ok ? "Passed" : "Needs attention"}
                  </span>
                </div>
              ))
            : [
                "System keyring",
                "Encrypted local storage",
                "Message viewer isolation",
              ].map((name, index) => (
                <div className="check-row" key={name}>
                  <span className="check-icon">
                    <Icon
                      name={
                        index === 0
                          ? "lock"
                          : index === 1
                            ? "monitor"
                            : "shield"
                      }
                      size={19}
                    />
                  </span>
                  <div className="check-copy">
                    <strong>{name}</strong>
                    <p>
                      {desktop
                        ? report.isFetching
                          ? "Checking this device…"
                          : "No result available. Re-run checks to try again."
                        : "Available in the desktop app"}
                    </p>
                  </div>
                  <span className="status-pill">
                    {desktop && report.isFetching ? "Checking" : "Not checked"}
                  </span>
                </div>
              ))}
        </div>
      </section>
      <section className="viewer-section" aria-labelledby="viewer-tests">
        <div className="section-heading">
          <h2 id="viewer-tests">Message viewer isolation</h2>
          <Icon name="shield" size={16} />
        </div>
        <p className="section-description">
          Email content opens in a separate, restricted window. These tests help
          verify that it stays separate from your application.
        </p>
        <div className="viewer-actions">
          <div>
            <span className="surface-icon">
              <Icon name="mail" size={20} />
            </span>
            <h3>Sanitised message</h3>
            <p>
              Inspect a hostile email fixture with scripts, forms, and remote
              images removed.
            </p>
            <button
              type="button"
              className="button button-secondary"
              disabled={!desktop || viewer.isPending}
              onClick={() => viewer.mutate()}
            >
              {viewer.isPending ? "Opening…" : "Open sanitised fixture"}
              <Icon name="chevron" size={14} />
            </button>
          </div>
          <div>
            <span className="surface-icon">
              <Icon name="shield" size={20} />
            </span>
            <h3>Isolation probe</h3>
            <p>
              Try native commands from an untrusted viewer. Every attempt must
              be denied.
            </p>
            <button
              type="button"
              className="button button-secondary"
              disabled={!desktop || probe.isPending}
              onClick={() => probe.mutate()}
            >
              {probe.isPending ? "Opening…" : "Open isolation probe"}
              <Icon name="chevron" size={14} />
            </button>
          </div>
        </div>
        {(viewer.isError || probe.isError) && (
          <div className="notice notice-error" role="alert">
            <Icon name="alert" />
            <p>
              Could not open the viewer: {String(viewer.error ?? probe.error)}
            </p>
          </div>
        )}
        {(viewer.isSuccess || probe.isSuccess) && (
          <p className="inline-success" role="status">
            <Icon name="checkCircle" size={16} />
            Viewer opened in a separate window. Re-run checks after the probe
            finishes.
          </p>
        )}
      </section>
    </section>
  );
}
