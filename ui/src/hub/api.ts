// Typed wrappers over the Tauri command surface. In a plain browser (vite dev
// without the shell) the invoke import fails and we fall back to defaults so the
// Hub still renders for iteration.

export type CleanupMode = "raw" | "local" | "open_ai";

/** Where speech recognition runs. Separate from CleanupMode: cleanup uploads a
 *  transcript, ASR uploads the recording. */
export type AsrMode = "local" | "cloud";
/** Rust aliases the retired "medium" and "high" onto "light" when loading. */
export type CleanupLevel = "none" | "messaging" | "light";
/** How the dictation key starts/stops a recording. */
export type TriggerMode = "hold" | "toggle" | "double_tap";

export interface Settings {
  cleanup_mode: CleanupMode;
  /** Where recognition runs. "cloud" uploads the audio; "local" never does. */
  asr_mode: AsrMode;
  cleanup_level: CleanupLevel;
  // "hold": hold the key while speaking, release to finish.
  // "toggle": press once to start, press again to stop.
  trigger_mode: TriggerMode;
  openai_model: string;
  // API root for the Cloud mode. Groq by default; any OpenAI-compatible endpoint
  // works, and blank means OpenAI itself. The `open_ai` mode name is the protocol,
  // not the vendor — it is in every saved settings.json.
  openai_base_url: string;
  sound_on_start: boolean;
  // Keep the raw pre-cleanup transcript next to the cleaned text, on this machine.
  // It is what speaking insights are computed from — fillers and self-corrections
  // only exist before cleanup deletes them.
  store_raw_transcripts: boolean;
  // Hub appearance. The overlay pill stays dark in every mode — it is drawn over
  // other people's windows, not inside one whose background was chosen here.
  appearance: Appearance;
}

// Imported for use in `Settings` below and re-exported so callers get it from the
// same module as the rest of the settings types.
import type { Appearance } from "./theme";
export type { Appearance };

export type LocalModelState = "loading" | "ready" | "missing";

/**
 * What macOS does when Fn is pressed on its own, independently of WhimprFlow.
 * Anything but "do_nothing" fires on top of dictation — most people meet this as
 * "the emoji picker opens every time I dictate". "unknown" means the setting has
 * never been touched, which is macOS's default, which is usually the emoji picker.
 */
export type FnKeyAction = "do_nothing" | "input_source" | "emoji" | "dictation" | "unknown";

/** Nothing granted, nothing loaded — the pre-load and browser-preview fallback. */
export const EMPTY_STATUS: Status = {
  accessibility: false,
  microphone: false,
  input_monitoring: false,
  has_openai_key: false,
  openai_key_count: 0,
  local_state: "loading",
  local_model: null,
  asr_model: null,
  // Assume the key is fine until the backend says otherwise, so the setup wizard
  // never flashes a warning at someone whose Fn key was never a problem.
  fn_key_action: "do_nothing",
};

export interface Status {
  accessibility: boolean;
  microphone: boolean;
  input_monitoring: boolean;
  has_openai_key: boolean;
  /** Stored cloud keys. More than one rotates when one is rate limited. */
  openai_key_count: number;
  /** Load state of the on-device cleanup model. */
  local_state: LocalModelState;
  /** GGUF filename in use, when `local_state` is "ready". */
  local_model: string | null;
  /** Whisper model filename on disk, if any. */
  asr_model: string | null;
  /** macOS's own action for a lone Fn press — see {@link FnKeyAction}. */
  fn_key_action: FnKeyAction;
}

/**
 * Turn a model filename into something readable:
 * `qwen3-4b-instruct-2507-q4_k_m.gguf` -> `Qwen3-4B-Instruct 2507 · Q4_K_M`
 * `ggml-base.en.bin`                   -> `Base.en`
 * Falls back to the bare filename for anything it doesn't recognise, so a model
 * this never anticipated still displays its real name rather than nothing.
 */
export function modelLabel(filename: string): string {
  const stem = filename.replace(/\.(gguf|bin)$/i, "").replace(/^ggml-/i, "");
  // Quantisation suffix (q4_k_m, q5_0, f16…) is the last meaningful chunk.
  const quant = stem.match(/[-_](f16|f32|q\d+(?:_[a-z0-9]+)*)$/i);
  const base = quant ? stem.slice(0, -quant[0].length) : stem;
  const words = base.split(/[-_]/).filter(Boolean).map((w) => {
    if (/^\d+b$/i.test(w)) return w.toUpperCase(); // 4b -> 4B
    if (/^\d+$/.test(w)) return w; // release tags like 2507
    return w.charAt(0).toUpperCase() + w.slice(1);
  });
  // A trailing bare number is a release tag ("2507"), which reads better spaced
  // off the model name than hyphenated into it.
  const tag = words.length > 1 && /^\d+$/.test(words[words.length - 1]) ? words.pop() : null;
  const label = words.join("-") + (tag ? ` ${tag}` : "");
  return quant ? `${label} · ${quant[1].toUpperCase()}` : label;
}

export interface StatsSummary {
  total_words: number;
  total_sessions: number;
  total_speaking_secs: number;
  avg_wpm: number;
  best_wpm: number;
  words_today: number;
  wpm_today: number;
  day_streak: number;
  time_saved_secs: number;
  last7_words: number[];
}

export const EMPTY_STATS: StatsSummary = {
  total_words: 0,
  total_sessions: 0,
  total_speaking_secs: 0,
  avg_wpm: 0,
  best_wpm: 0,
  words_today: 0,
  wpm_today: 0,
  day_streak: 0,
  time_saved_secs: 0,
  last7_words: [0, 0, 0, 0, 0, 0, 0],
};

export const DEFAULT_SETTINGS: Settings = {
  // Matches Rust's `Settings::default`, so the pre-load render and the browser
  // preview don't advertise an engine the app isn't actually using.
  cleanup_mode: "local",
  asr_mode: "local",
  cleanup_level: "light",
  trigger_mode: "hold",
  openai_model: "openai/gpt-oss-20b",
  openai_base_url: "https://api.groq.com/openai/v1",
  sound_on_start: true,
  store_raw_transcripts: true,
  appearance: "system",
};

async function invoke<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}

export async function getSettings(): Promise<Settings> {
  try {
    return await invoke<Settings>("get_settings");
  } catch {
    return DEFAULT_SETTINGS;
  }
}

export async function setSettings(settings: Settings): Promise<void> {
  try {
    await invoke<void>("set_settings", { settings });
  } catch {
    /* browser preview — no-op */
  }
}

/**
 * Settings changed somewhere other than this window — today that means the tray's
 * quick-settings menu. Without this the Hub keeps rendering whatever it last
 * fetched, so flipping Auto Cleanup from the tray leaves the pane insisting the old
 * level is still selected.
 *
 * Not fired for the Hub's own saves; echoing those back would round-trip every
 * keystroke in a text field through the backend.
 */
export async function onSettingsChanged(cb: (s: Settings) => void): Promise<() => void> {
  try {
    const { listen } = await import("@tauri-apps/api/event");
    return await listen<Settings>("whimpr://settings", (e) => cb(e.payload));
  } catch {
    return () => {}; // browser preview — no backend to hear from
  }
}

export async function getStatus(): Promise<Status> {
  try {
    return await invoke<Status>("get_status");
  } catch {
    // Browser preview: no backend to ask, so nothing is loaded.
    return { ...EMPTY_STATUS, local_state: "missing" };
  }
}

export async function getStats(): Promise<StatsSummary> {
  try {
    const tz = new Date().getTimezoneOffset(); // minutes to add to local -> UTC
    return await invoke<StatsSummary>("get_stats", { tzOffsetMinutes: tz });
  } catch {
    return EMPTY_STATS;
  }
}

export async function requestMicrophone(): Promise<void> {
  try {
    await invoke<void>("request_microphone");
  } catch {
    /* browser preview */
  }
}

export async function requestAccessibility(): Promise<void> {
  try {
    await invoke<void>("request_accessibility");
  } catch {
    /* browser preview */
  }
}

export async function requestInputMonitoring(): Promise<void> {
  try {
    await invoke<void>("request_input_monitoring");
  } catch {
    /* browser preview */
  }
}

/**
 * Open macOS Keyboard settings, where "Press 🌐 key to" lives. WhimprFlow can't
 * change it for you — the Fn action belongs to macOS, and our key tap only listens.
 */
export async function openKeyboardSettings(): Promise<void> {
  try {
    await invoke<void>("open_keyboard_settings");
  } catch {
    /* browser preview */
  }
}

/**
 * Reveal the diagnostics log in Finder.
 *
 * The one thing worth clicking when something is wrong and nothing on screen says
 * why: every decision the app made is in there with a timestamp, so it can be read
 * or handed to someone who can read it, without a terminal.
 */
export async function revealLogs(): Promise<void> {
  try {
    await invoke<void>("reveal_logs");
  } catch {
    /* browser preview */
  }
}

/** The stored keys, masked to their ends. The keys never reach the webview. */
export async function listApiKeys(provider: "openai"): Promise<string[]> {
  try {
    return await invoke<string[]>("list_api_keys", { provider });
  } catch {
    return []; /* browser preview */
  }
}

export async function addApiKey(provider: "openai", key: string): Promise<void> {
  try {
    await invoke<void>("add_api_key", { provider, key });
  } catch {
    /* browser preview */
  }
}

export async function removeApiKey(provider: "openai", index: number): Promise<void> {
  try {
    await invoke<void>("remove_api_key", { provider, index });
  } catch {
    /* browser preview */
  }
}

// ── History ────────────────────────────────────────────────────────────────
export interface HistoryItem {
  ts_unix: number;
  text: string;
  app: string | null;
  words: number;
}

/** Which slice of the history to fetch. Filtering and paging happen in Rust. */
export interface HistoryQuery {
  /** Case-insensitive substring over the dictated text; "" matches everything. */
  search: string;
  /** Unix seconds lower bound, 0 for no bound. Computed here so day boundaries
   *  follow the user's own clock rather than UTC. */
  since_unix: number;
  offset: number;
  limit: number;
}

export interface HistoryPage {
  items: HistoryItem[];
  /** Every match, not just this page — drives "11–20 of 347" and the Next button. */
  total: number;
}

export const EMPTY_HISTORY_PAGE: HistoryPage = { items: [], total: 0 };

export async function getHistory(query: HistoryQuery): Promise<HistoryPage> {
  try {
    return await invoke<HistoryPage>("get_history", { query });
  } catch {
    return EMPTY_HISTORY_PAGE;
  }
}

/** Erase the stored text of every dictation. Word counts and streaks survive. */
export async function clearTranscripts(): Promise<void> {
  try {
    await invoke<void>("clear_transcripts");
  } catch {
    /* browser preview — no-op */
  }
}

// ── Dictionary ───────────────────────────────────────────────────────────────
export interface DictEntry {
  correct: string;
  mishears: string[];
  auto: boolean;
}

export async function getDictionary(): Promise<DictEntry[]> {
  try {
    return await invoke<DictEntry[]>("get_dictionary");
  } catch {
    return [];
  }
}

export async function addDictionaryEntry(correct: string, mishears: string[]): Promise<void> {
  try {
    await invoke<void>("add_dictionary_entry", { correct, mishears });
  } catch {
    /* browser preview — no-op */
  }
}

export async function removeDictionaryEntry(correct: string): Promise<void> {
  try {
    await invoke<void>("remove_dictionary_entry", { correct });
  } catch {
    /* browser preview — no-op */
  }
}

