import Foundation

/// The channel between the keyboard extension and the app.
///
/// # Why there is a channel at all
///
/// A keyboard extension cannot record audio. This is not a permission that can be
/// granted: app extensions have no microphone entitlement, and iOS refuses the
/// capture outright. So the keyboard asks, the app records and transcribes, and the
/// keyboard inserts what comes back.
///
/// # Two transports, on purpose
///
/// The App Group container carries the *data* — it is the only memory both processes
/// can see. Darwin notifications carry the *timing*: they are the sole cross-process
/// wake-up an extension can receive, but they cannot carry a payload, so each one
/// means only "look in the container". Neither alone is enough.
///
/// Both need Allow Full Access. Without it the keyboard's view of the container is
/// empty rather than an error, which is why `Handoff.isReachable` exists.
extension Handoff {
    /// The URL the keyboard opens to reach the app when the background session is
    /// not alive. See `Info.plist`'s `CFBundleURLTypes`.
    static let dictateURL = URL(string: "whimprflow://dictate")!

    /// The URL the keyboard's menu opens to reach the app's settings.
    static let settingsURL = URL(string: "whimprflow://settings")!

    // MARK: - Shared state

    /// The container both processes read and write. `nil` when the keyboard has not
    /// been granted Allow Full Access.
    static var defaults: UserDefaults? {
        UserDefaults(suiteName: appGroup)
    }

    /// Whether the shared container is actually readable from here. In the keyboard
    /// this is false until the user turns on Allow Full Access, and it is the one
    /// thing worth checking before showing a mic key that cannot work.
    static var isReachable: Bool { defaults != nil }

    private enum Key {
        static let resultText = "result.text"
        static let resultID = "result.id"
        static let resultEngine = "result.engine"
        static let resultDegraded = "result.degraded"
        static let state = "state"
        static let aliveAt = "alive.at"
        static let inputName = "input.name"
        static let captureStartedAt = "capture.startedAt"
    }

    // MARK: - Result

    /// One finished dictation, waiting to be inserted.
    struct Result {
        let id: Int
        let text: String
        let engine: String
        let degraded: String?
    }

    /// Publish a finished dictation and wake the keyboard.
    ///
    /// The id is what makes insertion exactly-once: the keyboard records the last id
    /// it inserted, so a redelivered notification — or a keyboard that appears after
    /// the fact — inserts the text once and only once.
    static func publish(_ finished: Finished) {
        guard let defaults else { return }
        let id = defaults.integer(forKey: Key.resultID) + 1
        defaults.set(finished.text, forKey: Key.resultText)
        defaults.set(finished.engine.rawValue, forKey: Key.resultEngine)
        defaults.set(finished.degraded, forKey: Key.resultDegraded)
        defaults.set(id, forKey: Key.resultID)
        post(.result)
    }

    /// The most recent result, or nil if there has never been one.
    static func latestResult() -> Result? {
        guard let defaults else { return nil }
        let id = defaults.integer(forKey: Key.resultID)
        guard id > 0, let text = defaults.string(forKey: Key.resultText) else { return nil }
        return Result(
            id: id,
            text: text,
            engine: defaults.string(forKey: Key.resultEngine) ?? "raw",
            degraded: defaults.string(forKey: Key.resultDegraded)
        )
    }

    // MARK: - State

    /// What the app is doing, so the keyboard's mic key can show it.
    enum State: String {
        case idle, recording, transcribing, failed
    }

    static var state: State {
        get { State(rawValue: defaults?.string(forKey: Key.state) ?? "") ?? .idle }
        set {
            defaults?.set(newValue.rawValue, forKey: Key.state)
            post(.state)
        }
    }

    // MARK: - The capture in progress

    /// The microphone being recorded from, by the name iOS gives the route ("iPhone
    /// Microphone", "AirPods Pro"). Written by the app when a dictation starts; the
    /// keyboard cannot see another process's audio session.
    static var inputName: String? {
        get { defaults?.string(forKey: Key.inputName) }
        set { defaults?.set(newValue, forKey: Key.inputName) }
    }

    /// When the current dictation began, so the keyboard can show elapsed time
    /// without being told a tick at a time. Nil when nothing is being recorded.
    static var captureStartedAt: Date? {
        get {
            guard let at = defaults?.double(forKey: Key.captureStartedAt), at > 0 else { return nil }
            return Date(timeIntervalSince1970: at)
        }
        set { defaults?.set(newValue?.timeIntervalSince1970 ?? 0, forKey: Key.captureStartedAt) }
    }

    // MARK: - Liveness

    /// How recently the app confirmed it holds a live capture session.
    ///
    /// The keyboard uses this to choose between signalling in place and opening the
    /// app. It is a heartbeat rather than a flag because the app can be killed
    /// without getting to clear a flag, and a stale `true` would leave the mic key
    /// silently doing nothing — the exact failure the no-bounce design risks.
    static func markAlive() {
        defaults?.set(Date().timeIntervalSince1970, forKey: Key.aliveAt)
        post(.alive)
    }

    static func clearAlive() {
        defaults?.removeObject(forKey: Key.aliveAt)
        post(.alive)
    }

    /// Generous enough to survive a slow foreground tick, short enough that a killed
    /// app is noticed within one dictation's worth of hesitation.
    static let livenessWindow: TimeInterval = 12

    static var isAppLive: Bool {
        guard let at = defaults?.double(forKey: Key.aliveAt), at > 0 else { return false }
        return Date().timeIntervalSince1970 - at < livenessWindow
    }
}
