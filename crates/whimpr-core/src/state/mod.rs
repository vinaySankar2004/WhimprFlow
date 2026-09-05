//! The dictation state machine and its inputs/outputs.

pub mod actions;
pub mod events;
pub mod machine;
pub mod timing;
pub mod trigger;

pub use actions::{Action, BarState};
pub use events::{Input, PipelineEvent, TriggerToken};
pub use machine::{DictationState, StateMachine};
pub use trigger::{classify_double_tap_release, TapOutcome};
