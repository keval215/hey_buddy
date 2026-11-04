import { useEffect, useRef, useState } from "react";
import { on } from "../api/client";
import {
  MOVE_MS,
  isForDisplay,
  markerAt,
  opacityAt,
  toViewport,
  type DisplayGeom,
  type Pt,
} from "./marker";

/**
 * Cursor marker for one display. Listens to `turn:point` only (the overlay webview has no other
 * permission), eases to the point and fades out ~3 s later. The label is model output: it is
 * rendered as plain text by React and never interpreted.
 */
export function Overlay({ display }: { display: DisplayGeom }) {
  const markerRef = useRef<HTMLDivElement>(null);
  const [label, setLabel] = useState("");

  useEffect(() => {
    let raf = 0;
    let from: Pt = { x: 0, y: 0 };
    let to: Pt = { x: 0, y: 0 };
    let moveStart = 0;
    let shownAt = -Infinity;
    let pos: Pt = { x: 0, y: 0 };

    const frame = (now: number) => {
      const el = markerRef.current;
      const since = now - shownAt;
      const opacity = opacityAt(since);
      if (el) {
        pos = markerAt(from, to, now - moveStart);
        el.style.transform = `translate(${pos.x}px, ${pos.y}px)`;
        el.style.opacity = String(opacity);
      }
      if (opacity > 0) raf = requestAnimationFrame(frame);
      else raf = 0;
    };

    const unlisten = on("turn:point", (p) => {
      if (!isForDisplay(p, display)) return;
      const target = toViewport(p, display, { w: window.innerWidth, h: window.innerHeight });
      const now = performance.now();
      const visible = opacityAt(now - shownAt) > 0;
      from = visible ? pos : target;
      to = target;
      moveStart = now;
      shownAt = now + MOVE_MS; // the hold time starts when the marker arrives
      setLabel(p.label);
      if (raf === 0) raf = requestAnimationFrame(frame);
    });

    return () => {
      cancelAnimationFrame(raf);
      void unlisten.then((f) => f());
    };
  }, [display]);

  return (
    <div style={{ position: "fixed", inset: 0, pointerEvents: "none", overflow: "hidden" }}>
      <div
        ref={markerRef}
        style={{ position: "absolute", left: 0, top: 0, opacity: 0, willChange: "transform" }}
      >
        <div
          style={{
            position: "absolute",
            left: -14,
            top: -14,
            width: 28,
            height: 28,
            borderRadius: "50%",
            border: "3px solid #ff3b6b",
            background: "rgba(255, 59, 107, 0.25)",
            boxShadow: "0 0 12px rgba(255, 59, 107, 0.8)",
          }}
        />
        {label !== "" && (
          <div
            style={{
              position: "absolute",
              left: 20,
              top: 12,
              maxWidth: 320,
              padding: "2px 8px",
              borderRadius: 6,
              background: "rgba(20, 20, 24, 0.85)",
              color: "#fff",
              font: "14px system-ui, sans-serif",
              whiteSpace: "nowrap",
              overflow: "hidden",
              textOverflow: "ellipsis",
            }}
          >
            {label}
          </div>
        )}
      </div>
    </div>
  );
}
