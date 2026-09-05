import { useState } from "react";
import { font, palette } from "../tokens/values";
import { theme } from "./theme";
import { Button, Card, Dot, PageTitle, Segmented } from "./ui";
import {
  clearTranscripts,
  modelLabel,
  openKeyboardSettings,
  requestAccessibility,
  requestInputMonitoring,
  requestMicrophone,
  setApiKey,
  type CleanupLevel,
  type AsrMode,
  type CleanupMode,
  type FnKeyAction,
  type Settings,
  type Status,
  type TriggerMode,
} from "./api";

const MODES: { value: CleanupMode; label: string; hint: string }[] = [
  { value: "raw", label: "Raw", hint: "Paste exactly what you said" },
  // The Local hint is replaced at render time by the model actually loaded; this
  // string only shows if the status call never resolves.
  { value: "local", label: "Local", hint: "On-device model (offline)" },
  { value: "open_ai", label: "Cloud", hint: "Cloud cleanup — several times faster than on-device. Groq by default; any OpenAI-compatible API works" },
];

/**
 * These must stay in step with `GROQ_BASE_URL` / `GROQ_MODEL` in
 * `crates/whimpr-core/src/settings.rs`, which is where a fresh install gets its
 * defaults from. Both endpoints speak the OpenAI chat-completions format, so
 * switching between them is a URL and a model string — no new provider code.
 *
 * Model ids here are the *current* free-tier picks and they do rot: Groq announced
 * the deprecation of the llama-3.x ids in June 2026, and OpenRouter's `:free`
 * roster turns over constantly, which is why the OpenRouter entry uses the
 * auto-routing `openrouter/free` rather than pinning a model that will vanish.
 */
const GROQ_BASE_URL = "https://api.groq.com/openai/v1";
const GROQ_MODEL = "openai/gpt-oss-20b";
const ENDPOINTS: { label: string; baseUrl: string; model: string; hint: string }[] = [
  {
    label: "Groq",
    baseUrl: GROQ_BASE_URL,
    model: GROQ_MODEL,
    hint: "Fastest. Free: 30 requests/min, 1,000/day. Key from console.groq.com",
  },
  {
    label: "OpenRouter",
    baseUrl: "https://openrouter.ai/api/v1",
    model: "openrouter/free",
    hint: "Free: 50/day, or 1,000/day after a one-off $10. Slower — shared queue",
  },
  {
    label: "OpenAI",
    baseUrl: "",
    model: "gpt-4o-mini",
    hint: "Paid, billed per token",
  },
];

/** The short name the sidebar badge shows for the engine that is actually live. */
export function modeLabel(mode: CleanupMode): string {
  return MODES.find((m) => m.value === mode)?.label ?? mode;
}

const LEVELS: { value: CleanupLevel; label: string; hint: string }[] = [
  { value: "none", label: "None", hint: "Transcribe exactly what you said, including mistakes." },
  // Kept to roughly the length of the other two hints: the card wraps past about
  // 80 characters, and a two-line row among one-line rows reads as a mistake.
  {
    value: "messaging",
    label: "Messaging",
    hint: "Same cleanup, all lowercase, minimal punctuation.",
  },
  { value: "light", label: "Light", hint: "Clean up filler words and grammar. (Recommended)" },
];

const ASR_MODES: { value: AsrMode; label: string; hint: string }[] = [
  {
    value: "local",
    label: "On this Mac",
    hint: "Whisper runs on your GPU. Your audio never leaves the machine.",
  },
  {
    value: "cloud",
    label: "Cloud (faster)",
    hint: "The same Whisper model on Groq — roughly twice as fast. Uploads your audio.",
  },
];

const TRIGGERS: { value: TriggerMode; label: string; hint: string }[] = [
  {
    value: "hold",
    label: "Hold to talk",
    hint: "Hold Fn while you speak, release to finish. (Recommended)",
  },
  {
    value: "toggle",
    label: "Press to start, press to stop",
    hint: "Tap Fn once to start listening, tap it again to finish. Nothing to hold down.",
  },
];

/**
 * What macOS itself does with a lone Fn press. WhimprFlow can't change this — the
 * key tap only listens — so the row explains and opens the pane. "unknown" means
 * untouched, i.e. macOS's default, which on Apple keyboards is the emoji picker.
 */
const FN_KEY_DETAIL: Record<FnKeyAction, string> = {
  do_nothing: "set to Do Nothing — Fn is WhimprFlow's alone",
  emoji: "also opens the emoji picker — set it to Do Nothing (emoji stay on ⌃⌘Space)",
  input_source: "also switches keyboard input source — set it to Do Nothing",
  dictation: "also starts Apple dictation — set it to Do Nothing",
  unknown: "macOS default, usually the emoji picker — set it to Do Nothing",
};

/** A stack of radio-style option cards — one selected, each with a hint under it. */
function ChoiceList<T extends string>({
  options,
  value,
  onChange,
}: {
  options: { value: T; label: string; hint: string }[];
  value: T;
  onChange: (v: T) => void;
}) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      {options.map((o) => {
        const selected = value === o.value;
        return (
          <button
            key={o.value}
            onClick={() => onChange(o.value)}
            style={{
              textAlign: "left",
              cursor: "pointer",
              borderRadius: 12,
              padding: "12px 14px",
              fontFamily: font.ui,
              background: selected ? theme.accentSoft : theme.cardBgSubtle,
              border: `1px solid ${selected ? theme.accentSoftBorder : theme.border}`,
              color: theme.textBody,
            }}
          >
            <div style={{ fontSize: 14, fontWeight: 600, color: theme.textStrong }}>{o.label}</div>
            <div style={{ fontSize: 12.5, color: theme.textMuted, marginTop: 2 }}>{o.hint}</div>
          </button>
        );
      })}
    </div>
  );
}

/**
 * The line under the Cleanup Engine tabs. Every mode but Local has a fixed
 * description; Local names the GGUF actually loaded, so the pane stops claiming
 * a generic "on-device model" regardless of what is really running.
 */
function LocalHint({ mode, status }: { mode: CleanupMode; status: Status }) {
  const base = { fontSize: 12.5, marginTop: 10 } as const;

  if (mode !== "local") {
    return (
      <div style={{ ...base, color: theme.textMuted }}>
        {MODES.find((m) => m.value === mode)?.hint}
      </div>
    );
  }

  if (status.local_state === "missing") {
    return (
      <div style={{ ...base, color: palette.warn }}>
        No local model found — add a .gguf to ~/Library/Application Support/WhimprFlow/models,
        then reopen WhimprFlow.
      </div>
    );
  }

  if (status.local_state === "loading" || !status.local_model) {
    return <div style={{ ...base, color: theme.textMuted }}>Loading model…</div>;
  }

  return (
    <div style={{ ...base, color: theme.textMuted }} title={status.local_model}>
      <strong style={{ color: theme.textBody, fontWeight: 600 }}>
        {modelLabel(status.local_model)}
      </strong>{" "}
      — running on-device
      {status.asr_model && (
        <span title={status.asr_model}>
          {" · speech recognition: "}
          {modelLabel(status.asr_model)}
        </span>
      )}
    </div>
  );
}

/**
 * The Cleanup Engine card. The tabs *browse* the engines; nothing changes until
 * "Use this engine" is pressed. Switching used to apply instantly, which read as
 * broken rather than fast — a tab that highlights the moment you touch it looks
 * identical whether or not anything was saved. The commit step and the "In use"
 * marker make the live engine unambiguous, and the sidebar badge agrees with them.
 *
 * Only the browsed engine's own fields are shown. Raw and Local have no key to
 * set, and an API key field under "Paste exactly what you said" invites the
 * reasonable conclusion that Raw needs one.
 */
function EngineCard({
  settings,
  onChange,
  status,
  refresh,
}: {
  settings: Settings;
  onChange: (s: Settings) => void;
  status: Status;
  refresh: () => void;
}) {
  const [browsing, setBrowsing] = useState<CleanupMode>(settings.cleanup_mode);
  // Settings arrive from the backend after the first render, and can change from
  // elsewhere; follow them rather than stranding the tabs on a stale engine.
  const [lastLive, setLastLive] = useState<CleanupMode>(settings.cleanup_mode);
  if (lastLive !== settings.cleanup_mode) {
    setLastLive(settings.cleanup_mode);
    setBrowsing(settings.cleanup_mode);
  }

  const live = browsing === settings.cleanup_mode;
  // Cleanup falls back to the on-device model when a cloud key can't be read, so
  // say that here rather than letting it be discovered as "my cleanup is different".
  const missingKey = browsing === "open_ai" && !status.has_openai_key;

  return (
    <Card style={{ marginBottom: 16 }}>
      <SectionTitle sub="Where your dictation is cleaned up before it's typed.">Cleanup Engine</SectionTitle>
      <Segmented
        options={MODES.map((m) => ({ value: m.value, label: m.label }))}
        value={browsing}
        onChange={setBrowsing}
      />
      <LocalHint mode={browsing} status={status} />

      <div style={{ display: "flex", alignItems: "center", gap: 12, marginTop: 14 }}>
        {live ? (
          <span style={{ fontSize: 13, fontWeight: 600, color: theme.accentDeep }}>
            ✓ In use
          </span>
        ) : (
          <Button onClick={() => onChange({ ...settings, cleanup_mode: browsing })}>
            Use this engine
          </Button>
        )}
        {!live && (
          <span style={{ fontSize: 12.5, color: theme.textMuted }}>
            {modeLabel(settings.cleanup_mode)} is still doing the cleanup.
          </span>
        )}
      </div>
      {missingKey && (
        <div style={{ fontSize: 12.5, color: palette.warn, marginTop: 10 }}>
          No key set — this engine falls back to the on-device model, so cleanup still
          runs but not where you asked.
        </div>
      )}

      {browsing === "open_ai" && (
        <>
          <KeyField
            label="API key"
            configured={status.has_openai_key}
            onSave={(k) => {
              setApiKey("openai", k);
              setTimeout(refresh, 400);
            }}
          />
          <div style={{ marginTop: 12, display: "flex", gap: 6, alignItems: "center" }}>
            <span style={{ fontSize: 12.5, color: theme.textMuted }}>Preset:</span>
            {ENDPOINTS.map((p) => {
              const active = settings.openai_base_url === p.baseUrl;
              return (
                <button
                  key={p.label}
                  title={p.hint}
                  onClick={() =>
                    onChange({ ...settings, openai_base_url: p.baseUrl, openai_model: p.model })
                  }
                  style={{
                    ...presetStyle,
                    borderColor: active ? theme.accent : theme.border,
                    color: active ? theme.accent : theme.textMuted,
                  }}
                >
                  {p.label}
                </button>
              );
            })}
          </div>
          <div style={{ marginTop: 10, display: "flex", gap: 8 }}>
            <div style={{ flex: 1 }}>
              <div style={{ fontSize: 12.5, color: theme.textMuted, marginBottom: 6 }}>
                Base URL (blank = OpenAI)
              </div>
              <input
                type="text"
                value={settings.openai_base_url}
                placeholder={GROQ_BASE_URL}
                onChange={(e) => onChange({ ...settings, openai_base_url: e.target.value })}
                style={fieldStyle}
              />
            </div>
            <div style={{ flex: 1 }}>
              <div style={{ fontSize: 12.5, color: theme.textMuted, marginBottom: 6 }}>Model</div>
              <input
                type="text"
                value={settings.openai_model}
                placeholder={GROQ_MODEL}
                onChange={(e) => onChange({ ...settings, openai_model: e.target.value })}
                style={fieldStyle}
              />
            </div>
          </div>
        </>
      )}

    </Card>
  );
}

const presetStyle: React.CSSProperties = {
  background: "transparent",
  border: `1px solid ${theme.border}`,
  borderRadius: 6,
  padding: "3px 9px",
  fontSize: 12,
  cursor: "pointer",
};

const fieldStyle: React.CSSProperties = {
  width: "100%",
  background: theme.cardBgSubtle,
  border: `1px solid ${theme.border}`,
  borderRadius: 10,
  padding: "9px 12px",
  color: theme.textBody,
  fontFamily: font.mono,
  fontSize: 13,
  outline: "none",
  boxSizing: "border-box",
};

function SectionTitle({ children, sub }: { children: React.ReactNode; sub?: string }) {
  return (
    <div style={{ marginBottom: 14 }}>
      <div style={{ fontSize: 15, fontWeight: 600, color: theme.textStrong }}>{children}</div>
      {sub && <div style={{ color: theme.textMuted, fontSize: 13, marginTop: 4 }}>{sub}</div>}
    </div>
  );
}

function KeyField({
  label,
  configured,
  onSave,
}: {
  label: string;
  configured: boolean;
  onSave: (key: string) => void;
}) {
  const [value, setValue] = useState("");
  const [saved, setSaved] = useState(false);
  return (
    <div style={{ marginTop: 16 }}>
      <div style={{ fontSize: 13, marginBottom: 7, display: "flex", alignItems: "center", color: theme.textBody }}>
        <Dot ok={configured} />
        {label} {configured ? "— configured" : "— not set"}
      </div>
      <div style={{ display: "flex", gap: 8 }}>
        <input
          type="password"
          value={value}
          placeholder={configured ? "Enter a new key to replace" : "Paste your API key"}
          onChange={(e) => {
            setValue(e.target.value);
            setSaved(false);
          }}
          style={{
            flex: 1,
            background: theme.cardBgSubtle,
            border: `1px solid ${theme.border}`,
            borderRadius: 10,
            padding: "9px 12px",
            color: theme.textBody,
            fontFamily: font.mono,
            fontSize: 13,
            outline: "none",
          }}
        />
        <Button
          onClick={() => {
            onSave(value);
            setValue("");
            setSaved(true);
          }}
        >
          Save
        </Button>
      </div>
      {saved && <div style={{ fontSize: 12, color: theme.accentDeep, marginTop: 6 }}>Saved to keychain ✓</div>}
    </div>
  );
}

function PermRow({
  ok,
  label,
  detail,
  onClick,
  actionLabel = "Grant",
  okLabel = "Granted",
}: {
  ok: boolean;
  label: string;
  detail: string;
  onClick: () => void;
  /** Not every row is a permission grant — the Fn key row opens a settings pane. */
  actionLabel?: string;
  okLabel?: string;
}) {
  return (
    <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 12 }}>
      <div style={{ display: "flex", alignItems: "center", fontSize: 13 }}>
        <Dot ok={ok} />
        <span style={{ color: theme.textBody }}>
          <b>{label}</b> <span style={{ color: theme.textMuted }}>— {detail}</span>
        </span>
      </div>
      {ok ? (
        <span style={{ color: theme.accentDeep, fontSize: 13, fontWeight: 600 }}>{okLabel}</span>
      ) : (
        <Button variant="ghost" size="sm" onClick={onClick}>
          {actionLabel}
        </Button>
      )}
    </div>
  );
}

export function SettingsPane({
  settings,
  onChange,
  status,
  refresh,
}: {
  settings: Settings;
  onChange: (s: Settings) => void;
  status: Status;
  refresh: () => void;
}) {
  // Latches so the button reads "Cleared" rather than silently doing nothing the
  // second time — there is no other visible confirmation on this screen.
  const [cleared, setCleared] = useState(false);
  return (
    <div style={{ maxWidth: 720 }}>
      <PageTitle>Settings</PageTitle>

      <EngineCard settings={settings} onChange={onChange} status={status} refresh={refresh} />

      <Card style={{ marginBottom: 16 }}>
        <SectionTitle sub="Where your speech is turned into text. This runs before cleanup.">
          Speech Recognition
        </SectionTitle>
        <ChoiceList
          options={ASR_MODES}
          value={settings.asr_mode}
          onChange={(v) => onChange({ ...settings, asr_mode: v })}
        />
        {settings.asr_mode === "cloud" && !status.has_openai_key && (
          <div style={{ fontSize: 12.5, color: palette.warn, marginTop: 10 }}>
            No API key set — recognition will keep running on this Mac until you add one
            under Cleanup Engine. It uses the same Groq key.
          </div>
        )}
      </Card>

      <Card style={{ marginBottom: 16 }}>
        <SectionTitle>Auto Cleanup</SectionTitle>
        <ChoiceList
          options={LEVELS}
          value={settings.cleanup_level}
          onChange={(v) => onChange({ ...settings, cleanup_level: v })}
        />
      </Card>

      <Card style={{ marginBottom: 16 }}>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 12 }}>
          <div style={{ fontSize: 14, fontWeight: 600, color: theme.textStrong }}>
            Play a sound when recording starts
          </div>
          <Segmented
            options={[
              { value: "on", label: "On" },
              { value: "off", label: "Off" },
            ]}
            value={settings.sound_on_start ? "on" : "off"}
            onChange={(v) => onChange({ ...settings, sound_on_start: v === "on" })}
          />
        </div>
      </Card>

      <Card style={{ marginBottom: 16 }}>
        <SectionTitle sub="How the Fn key starts and stops a dictation.">Dictation Key</SectionTitle>
        <ChoiceList
          options={TRIGGERS}
          value={settings.trigger_mode}
          onChange={(v) => onChange({ ...settings, trigger_mode: v })}
        />
        <div style={{ fontSize: 12.5, color: theme.textMuted, marginTop: 10 }}>
          {settings.trigger_mode === "hold"
            ? "Tip: a quick double-tap of Fn also starts a hands-free session that keeps recording until you press Fn again."
            : "Recording continues after you let go of Fn — press it again to stop. It also stops on its own at the 20-minute session cap."}
        </div>
      </Card>

      <Card style={{ marginBottom: 16 }}>
        <SectionTitle sub="Your dictations are stored on this Mac and never uploaded.">
          History &amp; privacy
        </SectionTitle>
        <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 12 }}>
          <div style={{ maxWidth: 430 }}>
            <div style={{ fontSize: 14, fontWeight: 600, color: theme.textStrong }}>
              Keep the raw transcript
            </div>
            <div style={{ fontSize: 12.5, color: theme.textMuted, marginTop: 4, lineHeight: 1.5 }}>
              Saves what you said before cleanup tidied it. It is what speaking insights are
              built from — fillers and self-corrections only exist in the raw text.
            </div>
          </div>
          <Segmented
            options={[
              { value: "on", label: "On" },
              { value: "off", label: "Off" },
            ]}
            value={settings.store_raw_transcripts ? "on" : "off"}
            onChange={(v) => onChange({ ...settings, store_raw_transcripts: v === "on" })}
          />
        </div>
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "space-between",
            gap: 12,
            marginTop: 18,
            paddingTop: 16,
            borderTop: `1px solid ${theme.border}`,
          }}
        >
          <div style={{ maxWidth: 430 }}>
            <div style={{ fontSize: 14, fontWeight: 600, color: theme.textStrong }}>Clear transcripts</div>
            <div style={{ fontSize: 12.5, color: theme.textMuted, marginTop: 4, lineHeight: 1.5 }}>
              Erases the text of every past dictation. Your word count, WPM and streak are
              kept — they are counted, not quoted.
            </div>
          </div>
          <Button
            variant="ghost"
            onClick={() => {
              if (cleared) return;
              void clearTranscripts().then(() => setCleared(true));
            }}
          >
            {cleared ? "Cleared" : "Clear"}
          </Button>
        </div>
      </Card>

      <Card>
        <SectionTitle sub="Grant these to WhimprFlow, then quit and reopen the app if a dot stays grey.">
          Permissions
        </SectionTitle>
        <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
          <PermRow
            ok={status.accessibility}
            label="Accessibility"
            detail={
              status.accessibility
                ? "granted — Fn works everywhere + types your words"
                : "the key one: makes Fn work in EVERY app AND types your words"
            }
            onClick={() => {
              requestAccessibility();
              setTimeout(refresh, 800);
            }}
          />
          <PermRow
            ok={status.microphone}
            label="Microphone"
            detail={status.microphone ? "granted" : "hears what you say"}
            onClick={() => {
              requestMicrophone();
              setTimeout(refresh, 1000);
            }}
          />
          <PermRow
            ok={status.input_monitoring}
            label="Input Monitoring"
            detail="optional — extra reliability for key detection"
            onClick={() => {
              requestInputMonitoring();
              setTimeout(refresh, 1000);
            }}
          />
          {/* Not a permission, but it belongs here: it's the other macOS setting
              that decides whether pressing Fn does what you expect. */}
          <PermRow
            ok={status.fn_key_action === "do_nothing"}
            label="Fn key action"
            detail={FN_KEY_DETAIL[status.fn_key_action]}
            actionLabel="Open"
            okLabel="Off"
            onClick={() => {
              openKeyboardSettings();
              setTimeout(refresh, 1500);
            }}
          />
        </div>
      </Card>
    </div>
  );
}
