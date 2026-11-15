import { useEffect, useRef, useState } from "react";
import { closeMailBody, errorMessage, placeMailBody } from "../lib/native";

/** The native child has its own webview label and no native capabilities.
 * This element reserves space; it never contains email markup or an iframe.
 */
export function FormattedBody({ page, onRetry, text }: { page: string; onRetry: () => void; text: string }) {
  const host = useRef<HTMLDivElement>(null);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => {
    const element = host.current!;
    let disposed = false;
    let scheduled = 0;
    let placing = false;
    let dirty = false;
    const place = async () => {
      if (placing) { dirty = true; return; }
      placing = true;
      const rect = element.getBoundingClientRect();
      let top = Math.max(0, rect.top), bottom = Math.min(window.innerHeight, rect.bottom);
      let left = Math.max(0, rect.left), right = Math.min(window.innerWidth, rect.right);
      for (let parent = element.parentElement; parent; parent = parent.parentElement) {
        if (/(auto|scroll|hidden)/.test(getComputedStyle(parent).overflow)) {
          const clip = parent.getBoundingClientRect();
          top = Math.max(top, clip.top); bottom = Math.min(bottom, clip.bottom);
          left = Math.max(left, clip.left); right = Math.min(right, clip.right);
        }
      }
      // Native children sit above the app webview: hide them under app dialogs.
      const hidden = document.querySelector('[role="dialog"], dialog[open]') || right <= left || bottom <= top;
      try {
        await placeMailBody(page, hidden ? null : { x: left, y: top, width: right - left, height: bottom - top });
      } catch (failure) { if (!disposed) setError(failure); }
      finally {
        placing = false;
        if (disposed) void closeMailBody(page);
        else if (dirty) { dirty = false; schedule(); }
      }
    };
    const schedule = () => {
      cancelAnimationFrame(scheduled);
      scheduled = requestAnimationFrame(() => { void place(); });
    };
    const resize = new ResizeObserver(schedule);
    resize.observe(element);
    const mutations = new MutationObserver(schedule);
    mutations.observe(document.body, { childList: true, subtree: true, attributes: true, attributeFilter: ["open", "role", "class"] });
    window.addEventListener("resize", schedule);
    window.addEventListener("scroll", schedule, true);
    schedule();
    return () => {
      disposed = true;
      cancelAnimationFrame(scheduled);
      resize.disconnect(); mutations.disconnect();
      window.removeEventListener("resize", schedule);
      window.removeEventListener("scroll", schedule, true);
      void closeMailBody(page);
    };
  }, [page]);
  return <>
    {error != null && <>
      <p role="alert" className="mail-inline-error">{errorMessage(error)} <button type="button" className="button button-quiet" onClick={onRetry}>Try again</button></p>
      <div className="message-text">{text || "This message has no text body."}</div>
    </>}
    <div ref={host} role="region" aria-label="Email content" className="message-html" />
  </>;
}

