import Foundation

/// The declaration of `Handoff`, and the part of it every target needs: the App Group
/// name and the Darwin signals.
///
/// Split from `Handoff.swift` so the widget extension can compile it. The Live
/// Activity's buttons act by posting these signals — the same ones the keyboard
/// posts — and nothing else in `Handoff` (results, state, the core's `Finished`)
/// belongs in a widget. Compiling all of `Shared/` there would drag the bridge in.
enum Handoff {
    /// Must match the `com.apple.security.application-groups` entry in *both*
    /// entitlement files. A mismatch is silent: the container simply reads empty.
    static let appGroup = "group.com.whimpr.whimprflow"


    // MARK: - Signals

    /// Cross-process wake-ups. Payload-free by design — the data is in the container.
    enum Signal: String {
        /// Keyboard → app: begin a dictation.
        case start = "com.whimpr.whimprflow.dictate.start"
        /// Keyboard → app: the user tapped the mic key again; stop and transcribe.
        case stop = "com.whimpr.whimprflow.dictate.stop"
        /// Keyboard → app: throw the recording away without transcribing it.
        ///
        /// Distinct from `stop` rather than a flag beside it, because the two differ
        /// in what they cost: `stop` spends a recognition call and a cleanup call on
        /// audio the user has already decided against.
        case cancel = "com.whimpr.whimprflow.dictate.cancel"
        /// App → keyboard: a new result is in the container.
        case result = "com.whimpr.whimprflow.dictate.result"
        /// App → keyboard: `state` changed (recording, transcribing, failed).
        case state = "com.whimpr.whimprflow.dictate.state"
        /// App → keyboard: sent on every foreground tick while the app holds a live
        /// capture session, so the keyboard can tell whether tapping the mic key will
        /// work in place or needs to open the app.
        case alive = "com.whimpr.whimprflow.dictate.alive"
        /// Keyboard → app: a setting in the container changed (the level pill), so
        /// the app's cached copy must be re-read before the next dictation.
        case settings = "com.whimpr.whimprflow.settings.changed"
        /// Keyboard → app: release the microphone now — leave standby, end the
        /// session's Live Activity, let the indicator go off. The keyboard offers
        /// this because the indicator is seen from other apps, and the answer to
        /// "why is that on" should be one tap from where it is asked.
        case release = "com.whimpr.whimprflow.standby.release"
    }

    static func post(_ signal: Signal) {
        CFNotificationCenterPostNotification(
            CFNotificationCenterGetDarwinNotifyCenter(),
            CFNotificationName(signal.rawValue as CFString),
            nil, nil, true
        )
    }

    /// Observe a signal. The callback arrives on the main thread.
    ///
    /// `observer` must be a stable pointer for the lifetime of the observation — pass
    /// the object that owns it — because Darwin notifications carry no context and
    /// this is the only way to unregister precisely.
    static func observe(_ signal: Signal, observer: UnsafeRawPointer, callback: @escaping CFNotificationCallback) {
        CFNotificationCenterAddObserver(
            CFNotificationCenterGetDarwinNotifyCenter(),
            observer,
            callback,
            signal.rawValue as CFString,
            nil,
            .deliverImmediately
        )
    }

    static func stopObserving(observer: UnsafeRawPointer) {
        CFNotificationCenterRemoveEveryObserver(
            CFNotificationCenterGetDarwinNotifyCenter(),
            observer
        )
    }
}
