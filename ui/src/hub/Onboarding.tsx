import { useEffect, useState } from "react";
import { font, palette } from "../tokens/values";
import { theme } from "./theme";
import {
  openKeyboardSettings,
  requestAccessibility,
  requestMicrophone,
  requestInputMonitoring,
  addApiKey,
  type FnKeyAction,
  type Status,
} from "./api";

/**
 * What macOS will do on top of dictation, in words, or null when the key is free.
 * Only "do_nothing" is actually clear — an untouched setting is macOS's default,
 * which on Apple keyboards opens the emoji picker, so it is worth flagging too.
 */
function fnKeyClash(action: FnKeyAction): string | null {
  switch (action) {
    case "do_nothing":
      return null;
    case "emoji":
      return "Right now it also opens the emoji picker every time you dictate.";
    case "input_source":
      return "Right now it also switches your keyboard input source every time you dictate.";
    case "dictation":
      return "Right now it also starts Apple's own dictation every time you dictate.";
    default:
      return "It's set to macOS's default, which usually opens the emoji picker when you dictate.";
  }
}

// A blocking permission gate: the app can't be used until Accessibility and
// Microphone are granted. The three permissions are presented in order (each
// unlocks the next), and their state flips live as macOS applies them.

function Step({
  n,
  title,
  detail,
  done,
  active,
  locked,
  required,
  onGrant,
  actionLabel = "Grant",
  doneLabel = "Granted",
  control,
}: {
  n: number;
  title: string;
  detail: string;
  done: boolean;
  active: boolean;
  locked: boolean;
  required: boolean;
  onGrant?: () => void;
  /** Button text — not every step is a permission grant. */
  actionLabel?: string;
  doneLabel?: string;
  /** Replaces the button entirely. For a step that needs a field, not a grant. */
  control?: React.ReactNode;
}) {
  return (
    <div
      style={{
        display: "flex",
        alignItems: "center",
        gap: 16,
        padding: "16px 18px",
        borderRadius: 14,
        marginBottom: 12,
        background: active ? theme.accentSoft : theme.cardBg,
        border: `1px solid ${active ? theme.accentSoftBorder : theme.border}`,
        boxShadow: theme.shadowSoft,
        opacity: locked ? 0.5 : 1,
      }}
    >
      <div
        style={{
          flex: "0 0 auto",
          width: 30,
          height: 30,
          borderRadius: 9999,
          display: "flex",
          alignItems: "center",
          justifyContent: "center",
          fontWeight: 700,
          fontSize: 14,
          color: done ? "#fff" : theme.textMuted,
          background: done ? theme.accentDeep : theme.track,
        }}
      >
        {done ? "✓" : n}
      </div>
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ fontSize: 15, fontWeight: 600, color: theme.textStrong }}>
          {title}{" "}
          <span style={{ fontSize: 12, color: theme.textFaint, fontWeight: 400 }}>
            {required ? "· required" : "· optional"}
          </span>
        </div>
        <div style={{ fontSize: 13, color: theme.textMuted, marginTop: 2 }}>{detail}</div>
      </div>
      {done ? (
        <span style={{ color: theme.accentDeep, fontSize: 13, fontWeight: 600 }}>{doneLabel}</span>
      ) : control ? (
        control
      ) : (
        <button
          onClick={onGrant}
          disabled={locked}
          style={{
            cursor: locked ? "default" : "pointer",
            border: "none",
            borderRadius: 10,
            padding: "9px 16px",
            fontSize: 13,
            fontWeight: 600,
            fontFamily: font.ui,
            color: "#fff",
            background: locked ? theme.textFaint : palette.slate900,
            whiteSpace: "nowrap",
          }}
        >
          {actionLabel}
        </button>
      )}
    </div>
  );
}

/**
 * Paste-and-save for the Groq key, inline in the setup list.
 *
 * Inline rather than "go to Settings and find the Cleanup Engine pane": on a
 * cloud-only install this is a required step, and a required step that sends you
 * somewhere else to do it is one people stop at. The key never touches this
 * component's props or any file — `addApiKey` hands it straight to the Keychain.
 */
function KeyField({ onSaved, disabled }: { onSaved: () => void; disabled: boolean }) {
  const [value, setValue] = useState("");
  const [saving, setSaving] = useState(false);
  const trimmed = value.trim();

  async function save() {
    if (!trimmed || saving) return;
    setSaving(true);
    try {
      await addApiKey("openai", trimmed);
      // Clear it out of component state the moment it is stored; there is no reason
      // for the key to sit in the render tree afterwards.
      setValue("");
      onSaved();
    } finally {
      setSaving(false);
    }
  }

  return (
    <div style={{ display: "flex", gap: 8, alignItems: "center", flex: "0 0 auto" }}>
      <input
        type="password"
        value={value}
        disabled={disabled}
        placeholder="gsk_…"
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") void save();
        }}
        style={{
          width: 150,
          borderRadius: 10,
          border: `1px solid ${theme.border}`,
          background: theme.pageBg,
          color: theme.textBody,
          padding: "9px 10px",
          fontSize: 13,
          fontFamily: font.ui,
        }}
      />
      <button
        onClick={() => void save()}
        disabled={disabled || !trimmed || saving}
        style={{
          cursor: disabled || !trimmed ? "default" : "pointer",
          border: "none",
          borderRadius: 10,
          padding: "9px 14px",
          fontSize: 13,
          fontWeight: 600,
          fontFamily: font.ui,
          color: "#fff",
          background: disabled || !trimmed ? theme.textFaint : palette.slate900,
          whiteSpace: "nowrap",
        }}
      >
        {saving ? "Saving…" : "Save"}
      </button>
    </div>
  );
}

export function Onboarding({
  status,
  refresh,
  onEnter,
}: {
  status: Status;
  refresh: () => void;
  onEnter: () => void;
}) {
  // Poll live so the state flips the moment macOS applies each grant.
  useEffect(() => {
    const id = setInterval(refresh, 1200);
    return () => clearInterval(id);
  }, [refresh]);

  const acc = status.accessibility;
  const mic = status.microphone;
  const inp = status.input_monitoring;

  // No Whisper model on disk means this machine was installed cloud-only: there is
  // nothing to transcribe with locally, so an API key is not an enhancement here,
  // it is the difference between the app working and doing nothing at all. On an
  // install that did download the models this step never appears.
  const cloudOnly = status.asr_model === null;
  const needsKey = cloudOnly && !status.has_openai_key;
  const canEnter = acc && mic && !needsKey;

  // The Fn key step is a nag, so it only appears for people who actually have the
  // clash — but once shown it stays for the session, so fixing it ticks green here
  // instead of making the step silently vanish.
  const clash = fnKeyClash(status.fn_key_action);
  const [everClashed, setEverClashed] = useState(false);
  useEffect(() => {
    if (clash) setEverClashed(true);
  }, [clash]);
  const emojiish = status.fn_key_action === "emoji" || status.fn_key_action === "unknown";

  return (
    <div
      style={{
        height: "100vh",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        background: theme.pageBg,
        color: theme.textBody,
        fontFamily: font.ui,
        padding: 24,
      }}
    >
      <div style={{ width: 560, maxWidth: "100%" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginBottom: 6 }}>
          <div style={{ fontFamily: font.serif, fontSize: 30, fontWeight: 600, color: theme.textStrong }}>
            Set up WhimprFlow
          </div>
          <span
            style={{
              fontSize: 10,
              fontWeight: 700,
              letterSpacing: 0.4,
              textTransform: "uppercase",
              color: theme.accentDeep,
              background: theme.accentSoft,
              border: `1px solid ${theme.accentSoftBorder}`,
              borderRadius: 999,
              padding: "2px 7px",
            }}
          >
            {cloudOnly ? "Cloud" : "Local"}
          </span>
        </div>
        <p style={{ color: theme.textMuted, lineHeight: 1.5, margin: "0 0 24px" }}>
          Grant these to <b>WhimprFlow</b>, in order. Each turns green here the moment macOS applies
          it — no relaunch needed.
        </p>

        <Step
          n={1}
          title="Accessibility"
          detail="Detects the Fn key in every app and types your words. This is the one that makes the Fn key work everywhere."
          done={acc}
          active={!acc}
          locked={false}
          required
          onGrant={() => requestAccessibility()}
        />
        <Step
          n={2}
          title="Microphone"
          detail="Lets WhimprFlow hear what you say."
          done={mic}
          active={acc && !mic}
          locked={!acc}
          required
          onGrant={() => requestMicrophone()}
        />
        <Step
          n={3}
          title="Input Monitoring"
          detail="Extra reliability for key detection. Optional — you can enter without it."
          done={inp}
          active={acc && mic && !inp}
          locked={!(acc && mic)}
          required={false}
          onGrant={() => requestInputMonitoring()}
        />
        {cloudOnly && (
          <Step
            n={4}
            title="Groq API key"
            detail={
              status.has_openai_key
                ? "Saved to your macOS Keychain, never to a file."
                : "This Mac has no speech model downloaded, so WhimprFlow uses Groq for both steps. A key is free at console.groq.com — no card. It is stored in your macOS Keychain, never in a file."
            }
            done={status.has_openai_key}
            active={acc && mic && !status.has_openai_key}
            locked={!(acc && mic)}
            required
            doneLabel="Saved"
            control={<KeyField onSaved={refresh} disabled={!(acc && mic)} />}
          />
        )}
        {(clash || everClashed) && (
          <Step
            n={cloudOnly ? 5 : 4}
            title="Free up the Fn key"
            detail={
              clash
                ? `${clash} Set “Press 🌐 key to” to Do Nothing.${
                    emojiish ? " Emoji stay available on ⌃⌘Space." : ""
                  }`
                : "Set to Do Nothing — pressing Fn now only talks to WhimprFlow."
            }
            done={!clash}
            active={!!clash}
            locked={false}
            required={false}
            actionLabel="Open Settings"
            doneLabel="Off"
            onGrant={() => openKeyboardSettings()}
          />
        )}

        <button
          onClick={onEnter}
          disabled={!canEnter}
          style={{
            marginTop: 12,
            width: "100%",
            cursor: canEnter ? "pointer" : "default",
            border: "none",
            borderRadius: 12,
            padding: "13px",
            fontSize: 15,
            fontWeight: 700,
            fontFamily: font.ui,
            color: "#fff",
            background: canEnter ? theme.accentDeep : theme.textFaint,
          }}
        >
          {canEnter
            ? "Enter WhimprFlow →"
            : needsKey && acc && mic
              ? "Add your Groq key to continue"
              : "Grant Accessibility + Microphone to continue"}
        </button>

        <p style={{ fontSize: 12, color: theme.textFaint, lineHeight: 1.5, marginTop: 16 }}>
          If a permission stays grey after you flip it on in System Settings, toggle WhimprFlow off
          and back on in that pane — the state here will update within a second.
        </p>
      </div>
    </div>
  );
}
