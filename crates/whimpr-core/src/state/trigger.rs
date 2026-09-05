//! Classifying a dictation-key release in [`TriggerMode::DoubleTap`].
//!
//! [`TriggerMode`](crate::settings::TriggerMode) is a shell concern — it only decides
//! which binding a key press is reported as, and the state machine is unchanged by it.
//! But `DoubleTap` needs one genuinely fiddly judgement (was that a tap or a hold, and
//! did it pair with the last one), and that judgement is pure. So it lives here, where
//! it can be tested, rather than inside a CGEventTap callback where it cannot.
//!
//! The rules exist to protect the thing the mode is *for*: leaving the Fn key to macOS.
//! `Fn`+`Delete`, `Fn`+arrows and a lone press all have to keep working, so a release
//! only counts toward a double-tap when the press was short — holding Fn to reach
//! another key is a long press, and two quick forward-deletes must not read as a
//! gesture to start dictating into the document being edited.

use super::timing::{DOUBLE_TAP_MS, HOLD_MIN_MS};

/// What the shell should do about a dictation-key release in `DoubleTap` mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapOutcome {
    /// A double-tap completed: start a locked (hands-free) session.
    StartLocked,
    /// A lone tap. Remember this release time as the first of a possible pair.
    ArmFirstTap,
    /// Not a tap at all. Report nothing and forget any armed tap.
    Ignore,
}

/// Classify the release of the dictation key in `DoubleTap` mode.
///
/// `held_ms` is how long the key was down, `pending_tap_ms` the release time of an
/// already-armed first tap (`None` if none), and `at_ms` this release. All times come
/// from the shell's monotonic clock.
///
/// Only call this when no dictation is live — a press during one is the stop, which is
/// state and not timing, so the shell decides that before asking.
pub fn classify_double_tap_release(
    held_ms: u64,
    pending_tap_ms: Option<u64>,
    at_ms: u64,
) -> TapOutcome {
    // A hold is somebody using Fn as a modifier. It arms nothing, and it disarms:
    // otherwise `Fn+Delete, Fn+Delete` in quick succession pairs into a dictation.
    if held_ms >= HOLD_MIN_MS {
        return TapOutcome::Ignore;
    }
    match pending_tap_ms {
        Some(first) if at_ms.saturating_sub(first) <= DOUBLE_TAP_MS => TapOutcome::StartLocked,
        // Too slow to be a pair, so it becomes the first tap of the next one rather
        // than being discarded — otherwise a hesitant double-tap needs three presses.
        _ => TapOutcome::ArmFirstTap,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lone_quick_tap_only_arms() {
        assert_eq!(classify_double_tap_release(50, None, 1_000), TapOutcome::ArmFirstTap);
    }

    #[test]
    fn a_second_tap_inside_the_window_starts() {
        assert_eq!(
            classify_double_tap_release(50, Some(1_000), 1_000 + DOUBLE_TAP_MS),
            TapOutcome::StartLocked
        );
    }

    #[test]
    fn a_second_tap_past_the_window_becomes_the_next_first_tap() {
        // Re-arming rather than ignoring: a slow double-tap should take two more
        // presses at most, not three.
        assert_eq!(
            classify_double_tap_release(50, Some(1_000), 1_001 + DOUBLE_TAP_MS),
            TapOutcome::ArmFirstTap
        );
    }

    /// The whole reason the mode exists. Holding Fn to press Delete must not arm
    /// anything, and must clear anything already armed — otherwise two forward
    /// deletes in a row pair into a dictation started over the user's document.
    #[test]
    fn using_fn_as_a_modifier_neither_arms_nor_pairs() {
        assert_eq!(classify_double_tap_release(HOLD_MIN_MS, None, 1_000), TapOutcome::Ignore);
        assert_eq!(
            classify_double_tap_release(HOLD_MIN_MS + 400, Some(900), 1_000),
            TapOutcome::Ignore,
            "a hold must disarm, not pair"
        );
    }

    /// A tap is strictly shorter than `HOLD_MIN_MS`, matching how the state machine
    /// splits a tap from a hold in the other modes. The two must not disagree.
    #[test]
    fn the_tap_boundary_matches_the_state_machines() {
        assert_eq!(
            classify_double_tap_release(HOLD_MIN_MS - 1, None, 1_000),
            TapOutcome::ArmFirstTap
        );
        assert_eq!(classify_double_tap_release(HOLD_MIN_MS, None, 1_000), TapOutcome::Ignore);
    }
}
