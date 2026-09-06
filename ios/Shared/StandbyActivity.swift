import ActivityKit
import AppIntents
import Foundation

/// The Live Activity that stands for the standby session.
///
/// # Why there is one
///
/// Standby holds the microphone, and iOS shows the orange indicator for it. Shown
/// bare, that indicator is an accusation with no explanation — Wispr Flow's help
/// centre has an article for exactly that confusion. In the Dynamic Island the same
/// indicator sits beside our glyph, the expanded view says what the mic is doing,
/// and the buttons end it from wherever the question was asked.
///
/// Buttons are `LiveActivityIntent`s and act by posting the keyboard's own Darwin
/// signals, so the app has one set of handlers whichever surface asked.
struct StandbyActivityAttributes: ActivityAttributes {
    struct ContentState: Codable, Hashable {
        enum Phase: String, Codable {
            /// Holding the mic, discarding everything. Ready for the mic key.
            case ready
            case listening
            case transcribing
        }

        var phase: Phase
        /// The route being recorded from, as iOS names it.
        var inputName: String?
        /// When the current dictation began. Drives the elapsed-time counter.
        var startedAt: Date?
        /// When standby will release the mic if nothing happens. Nil for no limit.
        var releaseAt: Date?
    }
}

/// Finish the dictation in progress and transcribe it.
struct StopDictationIntent: LiveActivityIntent {
    static var title: LocalizedStringResource = "Finish dictation"
    static var isDiscoverable = false

    func perform() async throws -> some IntentResult {
        Handoff.post(.stop)
        return .result()
    }
}

/// Throw the dictation in progress away.
struct DiscardDictationIntent: LiveActivityIntent {
    static var title: LocalizedStringResource = "Discard dictation"
    static var isDiscoverable = false

    func perform() async throws -> some IntentResult {
        Handoff.post(.cancel)
        return .result()
    }
}

/// Leave standby: release the microphone, end the activity, indicator off.
struct ReleaseMicIntent: LiveActivityIntent {
    static var title: LocalizedStringResource = "Release the microphone"
    static var isDiscoverable = false

    func perform() async throws -> some IntentResult {
        Handoff.post(.release)
        return .result()
    }
}
