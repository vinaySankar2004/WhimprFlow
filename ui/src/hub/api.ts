// Typed wrappers over the Tauri command surface. In a plain browser (vite dev
// without the shell) the invoke import fails and we fall back to defaults so the
// Hub still renders for iteration.

export type CleanupMode = "raw" | "local" | "open_ai" | "anthropic";
export type CleanupLevel = "none" | "light" | "medium" | "high";

export interface Settings {
  cleanup_mode: CleanupMode;
  cleanup_level: CleanupLevel;
  openai_model: string;
  // API root for "OpenAI" mode — leave blank for OpenAI itself, or point at
  // an OpenAI-compatible endpoint like OpenRouter (https://openrouter.ai/api/v1).
  openai_base_url: string;
  anthropic_model: string;
  sound_on_start: boolean;
}

export type LocalModelState = "loading" | "ready" | "missing";

/** Nothing granted, nothing loaded — the pre-load and browser-preview fallback. */
export const EMPTY_STATUS: Status = {
  accessibility: false,
  microphone: false,
  input_monitoring: false,
  has_openai_key: false,
  has_anthropic_key: false,
  local_state: "loading",
  local_model: null,
  asr_model: null,
};

export interface Status {
  accessibility: boolean;
  microphone: boolean;
  input_monitoring: boolean;
  has_openai_key: boolean;
  has_anthropic_key: boolean;
  /** Load state of the on-device cleanup model. */
  local_state: LocalModelState;
  /** GGUF filename in use, when `local_state` is "ready". */
  local_model: string | null;
  /** Whisper model filename on disk, if any. */
  asr_model: string | null;
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
  cleanup_mode: "open_ai",
  cleanup_level: "light",
  openai_model: "gpt-4o-mini",
  openai_base_url: "",
  anthropic_model: "claude-haiku-4-5",
  sound_on_start: true,
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

export async function setApiKey(provider: "openai" | "anthropic", key: string): Promise<void> {
  try {
    await invoke<void>("set_api_key", { provider, key });
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

export async function getHistory(): Promise<HistoryItem[]> {
  try {
    return await invoke<HistoryItem[]>("get_history");
  } catch {
    return [];
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

