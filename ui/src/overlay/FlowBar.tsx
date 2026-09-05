import { useEffect, useRef, useState } from "react";
import { palette, pillFill, geometry, font } from "../tokens/values";

// Visual states, mirroring the Rust `BarState`.
export type BarState =
  | "idle"
  | "recording"
  | "locked"
  | "transcribing"
  | "done"
  | "cancelled"
  | "error";

type StateEvent = { state: BarState };
type WaveformEvent = { bars: number[] };
type NoticeEvent = { text: string };

async function tauriListen<T>(event: string, cb: (payload: T) => void): Promise<() => void> {
  try {
    const { listen } = await import("@tauri-apps/api/event");
    return await listen<T>(event, (e) => cb(e.payload as T));
  } catch {
    return () => {};
  }
}

/** Fire a Tauri command; a no-op in a plain browser preview. */
async function tauriInvoke(cmd: string): Promise<void> {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke(cmd);
  } catch {
    /* browser preview — no shell to talk to */
  }
}

// A row of dot-like rounded bars driven by mic RMS — Wispr's dotted-waveform look:
// small dots when quiet, rising into a waveform when speaking.
function DottedWaveform({ bars }: { bars: number[] }) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const barsRef = useRef<number[]>(bars);
  barsRef.current = bars;

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    let raf = 0;
    const N = 16;
    const draw = () => {
      const dpr = window.devicePixelRatio || 1;
      const w = canvas.clientWidth;
      const h = canvas.clientHeight;
      if (canvas.width !== w * dpr || canvas.height !== h * dpr) {
        canvas.width = w * dpr;
        canvas.height = h * dpr;
      }
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.clearRect(0, 0, w, h);
      const dotW = 2.4;
      const gap = (w - N * dotW) / (N - 1);
      const t = performance.now();
      ctx.fillStyle = palette.waveBar;
      for (let i = 0; i < N; i++) {
        const real = barsRef.current[barsRef.current.length - 1 - (i % barsRef.current.length)];
        // Idle shimmer so the dotted line reads as "listening" even in near-silence.
        const shimmer = 0.12 + 0.06 * Math.abs(Math.sin(t / 260 + i * 0.7));
        const amp = Math.max(shimmer, real ?? 0);
        const bh = 3 + amp * 20; // 3px dot → up to ~23px bar
        const x = i * (dotW + gap);
        const y = (h - bh) / 2;
        ctx.beginPath();
        ctx.roundRect(x, y, dotW, bh, dotW / 2);
        ctx.fill();
      }
      raf = requestAnimationFrame(draw);
    };
    raf = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(raf);
  }, []);

  return <canvas ref={canvasRef} style={{ width: "100%", height: 28 }} />;
}

// Both pill controls are real buttons on a NON-ACTIVATING panel: the click is
// delivered without the overlay taking focus, so the app being dictated into stays
// frontmost and the paste still lands in the right place. `onMouseDown` +
// preventDefault keeps the webview from trying to move focus at all.
const CONTROL_BASE = {
  flex: "0 0 auto",
  width: 26,
  height: 26,
  borderRadius: 9999,
  border: "none",
  padding: 0,
  cursor: "pointer",
  display: "flex",
  alignItems: "center",
  justifyContent: "center",
} as const;

function CancelButton() {
  return (
    <button
      title="Discard this dictation (Esc)"
      aria-label="Discard this dictation"
      onMouseDown={(e) => e.preventDefault()}
      onClick={() => void tauriInvoke("cancel_dictation")}
      style={{
        ...CONTROL_BASE,
        background: "rgba(255,255,255,0.16)",
        color: "#fff",
        fontSize: 15,
        lineHeight: 1,
      }}
    >
      ✕
    </button>
  );
}

function StopButton() {
  return (
    <button
      title="Stop and paste"
      aria-label="Stop and paste"
      onMouseDown={(e) => e.preventDefault()}
      onClick={() => void tauriInvoke("stop_dictation")}
      style={{ ...CONTROL_BASE, background: "#FF5A52" }}
    >
      <div style={{ width: 9, height: 9, borderRadius: 2, background: "#fff" }} />
    </button>
  );
}

// Keyframes for the states that need to move. Inline styles can't express
// @keyframes, and the overlay has no stylesheet, so the component carries its own.
const KEYFRAMES = `
@keyframes whimpr-pill-in {
  from { opacity: 0; transform: translateY(6px) scale(0.94); }
  to   { opacity: 1; transform: translateY(0) scale(1); }
}
@keyframes whimpr-live-pulse {
  0%, 100% { opacity: 1;   transform: scale(1); }
  50%      { opacity: 0.45; transform: scale(0.82); }
}
@keyframes whimpr-spin { to { transform: rotate(360deg); } }
`;

/** The "we are recording right now" tell: a pulsing red dot, like every record button. */
function LiveDot() {
  return (
    <span
      style={{
        flex: "0 0 auto",
        width: 8,
        height: 8,
        borderRadius: 9999,
        background: "#FF5A52",
        boxShadow: "0 0 0 3px rgba(255,90,82,0.22)",
        animation: "whimpr-live-pulse 1.1s ease-in-out infinite",
      }}
    />
  );
}

/** Indeterminate ring — makes "working" visually distinct from "listening". */
function Spinner() {
  return (
    <span
      style={{
        flex: "0 0 auto",
        width: 13,
        height: 13,
        borderRadius: 9999,
        border: `2px solid rgba(255,255,255,0.22)`,
        borderTopColor: palette.accent400,
        animation: "whimpr-spin 0.7s linear infinite",
      }}
    />
  );
}

export function FlowBar() {
  const [state, setState] = useState<BarState>("idle");
  const [bars, setBars] = useState<number[]>([]);
  // A locked hands-free session auto-stops at the cap. Without this the recording
  // just ends mid-sentence with no warning, which reads as a crash.
  const [nearCap, setNearCap] = useState(false);
  // A specific reason for an error, when the backend has one. "Something's off" is
  // fine for a one-off failure and useless for a misconfiguration, which recurs on
  // every attempt until the person is told what to fix.
  const [notice, setNotice] = useState<string | null>(null);

  useEffect(() => {
    // listen() resolves asynchronously, so a cleanup that runs before it settles
    // (StrictMode's double-mount) would otherwise leak a duplicate listener pair
    // and double-apply every state event.
    let cancelled = false;
    const unlisteners: Array<() => void> = [];
    const register = (p: Promise<() => void>) =>
      p.then((un) => (cancelled ? un() : unlisteners.push(un)));

    register(
      tauriListen<StateEvent>("whimpr://flowbar/state", (p) => {
        setState(p.state);
        // Any state change ends the session the warning was about.
        setNearCap(false);
        // The notice arrives immediately BEFORE the error state it explains, so it
        // is cleared on every other state rather than on all of them.
        if (p.state !== "error") setNotice(null);
      }),
    );
    register(
      tauriListen<NoticeEvent>("whimpr://flowbar/notice", (p) => setNotice(p.text)),
    );
    register(tauriListen<WaveformEvent>("whimpr://audio/waveform", (p) => setBars(p.bars)));
    register(tauriListen("whimpr://session-cap", () => setNearCap(true)));

    return () => {
      cancelled = true;
      unlisteners.forEach((un) => un());
    };
  }, []);

  const recording = state === "recording" || state === "locked";
  const isIdle = state === "idle";
  const processing = state === "transcribing";
  const statusText =
    state === "transcribing"
      ? "Cleaning up…"
      : state === "error"
        ? (notice ?? "Something's off")
        : state === "cancelled"
          ? "Discarded"
          : "Done";

  // Idle draws nothing at all. The old empty 76×16 pill was a near-black bar on a
  // dark desktop — invisible in practice, but still a permanent object on screen.
  if (isIdle) return null;

  // Transcribing keeps a ✕ (the pipeline can still be abandoned before it pastes),
  // so it needs room for a control the terminal states don't have.
  const dims = recording
    ? { w: nearCap ? 322 : 260, h: 46 } // the caption needs its own room, not the waveform's
    : processing
      ? { w: 226, h: 38 }
      : notice
        ? // A reason is a sentence, not a word. At 190px it truncates to something
          // shorter than "Something's off" and the whole point of it is lost.
          { w: 340, h: 38 }
        : { w: 190, h: 38 };

  return (
    <div
      style={{
        position: "fixed",
        inset: 0,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        fontFamily: font.ui,
        userSelect: "none",
      }}
    >
      <style>{KEYFRAMES}</style>
      <div
        aria-label={`WhimprFlow ${state}`}
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: recording ? "space-between" : "center",
          gap: 10,
          height: dims.h,
          width: dims.w,
          padding: recording ? "0 8px" : "0 12px",
          background: pillFill.base,
          // Recording gets an accent rim + glow so it reads as live from the corner
          // of the eye; the calmer states keep the plain hairline border.
          border: recording
            ? `1px solid ${palette.accent500}`
            : `1px solid rgba(255,255,255,0.10)`,
          borderRadius: 9999,
          boxShadow: recording
            ? `${pillFill.shadow}, 0 0 0 4px rgba(34,195,182,0.16), 0 0 18px rgba(34,195,182,0.28)`
            : pillFill.shadow,
          color: palette.pillText,
          transition: `width ${geometry.morphMs}ms ${motionEase}, height ${geometry.morphMs}ms ${motionEase}`,
          animation: `whimpr-pill-in ${geometry.morphMs}ms ${motionEase}`,
          overflow: "hidden",
          fontSize: 13,
        }}
      >
        {recording ? (
          <>
            <CancelButton />
            <LiveDot />
            <div style={{ flex: 1, minWidth: 0 }}>
              <DottedWaveform bars={bars} />
            </div>
            {nearCap && (
              <span style={{ color: palette.pillTextMuted, fontSize: 11, whiteSpace: "nowrap" }}>
                1 min left
              </span>
            )}
            <StopButton />
          </>
        ) : processing ? (
          <>
            <CancelButton />
            <Spinner />
            <span style={{ color: palette.pillTextMuted }}>{statusText}</span>
          </>
        ) : (
          <span style={{ color: palette.pillTextMuted }}>{statusText}</span>
        )}
      </div>
    </div>
  );
}

const motionEase = "cubic-bezier(0.05,0.6,0.4,0.95)";
