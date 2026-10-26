import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import {
  foundationReport,
  isDesktop,
  openViewerFixture,
  openViewerProbe,
} from "../lib/native";

export const Route = createFileRoute("/foundation")({ component: Foundation });

function Foundation() {
  const report = useQuery({
    queryKey: ["foundation-report"],
    queryFn: foundationReport,
    enabled: isDesktop(),
  });
  const viewer = useMutation({ mutationFn: openViewerFixture });
  const probe = useMutation({ mutationFn: openViewerProbe });

  if (!isDesktop()) {
    return (
      <section>
        <h1>Foundation checks</h1>
        <p>Not running inside the desktop shell; native checks are unavailable.</p>
      </section>
    );
  }

  return (
    <section>
      <h1>Foundation checks</h1>
      {report.isPending && <p>Running checks…</p>}
      {report.isError && <p className="bad">Failed: {String(report.error)}</p>}
      {report.data && (
        <table>
          <tbody>
            {report.data.checks.map((check) => (
              <tr key={check.name}>
                <td className={check.ok ? "ok" : "bad"}>{check.ok ? "pass" : "fail"}</td>
                <td>{check.name}</td>
                <td>{check.detail}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <h2>Message viewer isolation</h2>
      <p>
        The sanitised fixture shows how hostile mail renders: no scripts, no remote images,
        no forms. The probe assumes an attacker already runs script in a viewer and tries
        native commands; every attempt must be denied. Re-run the checks afterwards.
      </p>
      <div className="actions">
        <button type="button" onClick={() => viewer.mutate()} disabled={viewer.isPending}>
          Open sanitised fixture
        </button>
        <button type="button" onClick={() => probe.mutate()} disabled={probe.isPending}>
          Open isolation probe
        </button>
        <button type="button" onClick={() => report.refetch()}>
          Re-run checks
        </button>
      </div>
      {viewer.isError && <p className="bad">{String(viewer.error)}</p>}
      {probe.isError && <p className="bad">{String(probe.error)}</p>}
    </section>
  );
}
