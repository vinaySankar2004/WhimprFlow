//! `whimpr-core` — the pure brain of WhimprFlow.
//!
//! No I/O, no platform code, no GPU: the dictation [`state`] machine, the cleanup
//! prompts/levels/[`gates`](cleanup::gates), the [`dictionary`], [`settings`] and
//! [`stats`]. Native concerns (the CGEventTap, paste, accessibility reads) live in
//! `src-tauri`; the ASR and cleanup-LLM implementations live in their own crates and
//! plug in behind the [`asr`] and [`cleanup`] trait seams defined here.
//!
//! Because nothing here touches the world, this is where the tests are — and why
//! passing them is not on its own evidence that the app works.

pub mod asr;
pub mod cleanup;
pub mod dictionary;
pub mod settings;
pub mod state;
pub mod stats;
pub mod types;

pub use asr::{AsrEngine, AsrEngineId, Transcript};
pub use cleanup::{CleanupContext, CleanupLevel, CleanupProvider, ProviderId, VocabEntry};
pub use dictionary::{DictSource, DictionaryEntry, DictionaryStore};
pub use settings::{
    AsrMode, CleanupMode, Settings, TriggerMode, GROQ_ASR_MODEL, GROQ_ASR_URL, GROQ_BASE_URL,
    GROQ_MODEL,
};
pub use stats::{HistoryItem, HistoryPage, HistoryQuery, SessionRecord, StatsStore, StatsSummary};
pub use state::{Action, BarState, DictationState, Input, PipelineEvent, StateMachine, TriggerToken};
pub use types::{RecordMode, SessionId};
