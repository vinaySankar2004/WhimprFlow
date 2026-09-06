import Foundation

/// User settings and the dictionary, persisted in the App Group container.
///
/// Deliberately much smaller than the Mac's `Settings`: this build is cloud-only, so
/// every knob about local models, engines and fallbacks is absent rather than
/// present-and-ignored. Anything worth a switch here is worth one on the Mac too.
@Observable
final class Settings {
    static let shared = Settings()

    private let defaults = Handoff.defaults ?? .standard

    private enum Key {
        static let level = "settings.cleanupLevel"
        static let dictionary = "settings.dictionary"
        static let backgroundSession = "settings.backgroundSession"
        static let standbyTimeout = "settings.standbyTimeout"
        static let soundOnStart = "settings.soundOnStart"
        static let autocorrect = "settings.autocorrect"
        static let appearance = "settings.appearance"
    }

    // MARK: - Appearance

    /// Light, dark, or whatever the system is doing. Default is the system.
    var appearance: Appearance {
        didSet { defaults.set(appearance.rawValue, forKey: Key.appearance) }
    }

    /// The same value, readable without building a `Settings`.
    ///
    /// The keyboard needs the appearance and nothing else from here, and it is a
    /// separate process that cannot ask the app — it reads the shared container
    /// directly. Instantiating the observable object in an extension to get one enum
    /// would drag the dictionary and its JSON along for no reason.
    static var storedAppearance: Appearance {
        let raw = (Handoff.defaults ?? .standard).string(forKey: Key.appearance) ?? ""
        return Appearance(rawValue: raw) ?? .system
    }

    // MARK: - Cleanup level

    var level: CleanupLevel {
        didSet { defaults.set(level.rawValue, forKey: Key.level) }
    }

    /// The level as stored, readable without building a `Settings` — the keyboard's
    /// pill shows and changes it, from a process that cannot ask the app.
    static var storedLevel: CleanupLevel {
        get {
            let raw = (Handoff.defaults ?? .standard).string(forKey: Key.level) ?? ""
            return CleanupLevel(rawValue: raw) ?? .light
        }
        set {
            (Handoff.defaults ?? .standard).set(newValue.rawValue, forKey: Key.level)
        }
    }

    /// Pick up a change the keyboard made. `level` is cached in memory for the
    /// observation graph, so without this the app cleans the next dictation at the
    /// level the pill *used* to show.
    func reloadLevel() {
        let stored = Self.storedLevel
        if stored != level { level = stored }
    }

    // MARK: - Dictionary

    /// The authoritative spellings and their known mishears.
    ///
    /// Stored as the core's own JSON shape so it crosses the FFI unchanged and can be
    /// copied verbatim to and from the Mac's `dictionary.json`.
    var dictionary: DictionaryStore {
        didSet {
            let json = try? JSONSerialization.data(withJSONObject: dictionary.payload)
            defaults.set(json, forKey: Key.dictionary)
        }
    }

    // MARK: - Standby

    /// How long the app keeps the microphone ready after the last dictation.
    ///
    /// Ready means holding a live capture session, which is what lets the mic key
    /// dictate without a visible app switch — and what shows the orange microphone
    /// indicator. The timeout bounds that: after this long idle the mic is released,
    /// the indicator goes off, and the next mic-key tap opens the app once to re-arm.
    /// The same shape, and the same default, as Wispr Flow's session setting.
    var standbyTimeout: StandbyTimeout {
        didSet { defaults.set(standbyTimeout.rawValue, forKey: Key.standbyTimeout) }
    }

    /// Whether standby runs at all. Kept as a name because everything that gates
    /// standby asks this one question.
    var keepSessionAlive: Bool { standbyTimeout != .off }

    // MARK: - Typing

    /// Whether the keyboard corrects spelling as you type. Conservative by design —
    /// see `TypingEngine.autocorrect` — and honouring a field that turns it off.
    var autocorrect: Bool {
        didSet { defaults.set(autocorrect, forKey: Key.autocorrect) }
    }

    /// The same value for the keyboard, which reads the container directly.
    static var storedAutocorrect: Bool {
        (Handoff.defaults ?? .standard).object(forKey: Key.autocorrect) as? Bool ?? true
    }

    // MARK: - Sound

    /// The record-start pop. Mirrors the Mac's `sound_on_start`, default and all: the
    /// pop is how you know the mic opened without looking, which on a phone whose
    /// keyboard is the only thing on screen is most of the time.
    var soundOnStart: Bool {
        didSet { defaults.set(soundOnStart, forKey: Key.soundOnStart) }
    }

    private init() {
        level = CleanupLevel(rawValue: defaults.string(forKey: Key.level) ?? "") ?? .light
        if let raw = defaults.string(forKey: Key.standbyTimeout),
           let stored = StandbyTimeout(rawValue: raw) {
            standbyTimeout = stored
        } else if let legacy = defaults.object(forKey: Key.backgroundSession) as? Bool {
            // The switch this replaced: off stays off, on becomes the default timeout
            // rather than "always", which is the behaviour it had and the one people
            // asked to be rid of.
            standbyTimeout = legacy ? .fiveMinutes : .off
        } else {
            standbyTimeout = .fiveMinutes
        }
        appearance = Appearance(rawValue: defaults.string(forKey: Key.appearance) ?? "") ?? .system
        soundOnStart = defaults.object(forKey: Key.soundOnStart) as? Bool ?? true
        autocorrect = defaults.object(forKey: Key.autocorrect) as? Bool ?? true
        if let data = defaults.data(forKey: Key.dictionary),
           let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let entries = object["entries"] as? [[String: Any]] {
            dictionary = DictionaryStore(entries: entries)
        } else {
            dictionary = DictionaryStore()
        }
    }

    // MARK: - Endpoints

    /// Mirrors `whimpr_core::settings`. Kept as literals rather than read over the
    /// bridge because they are needed before any request is built, and a wrong URL
    /// fails loudly on the first call rather than silently.
    enum Groq {
        static let base = "https://api.groq.com/openai/v1"
        static let transcriptions = "\(base)/audio/transcriptions"
        static let chatCompletions = "\(base)/chat/completions"
        static let asrModel = "whisper-large-v3-turbo"
        static let cleanupModel = "openai/gpt-oss-120b"
    }
}

/// How long the mic stays ready after a dictation.
///
/// Raw values are stable strings, not seconds, so the stored preference survives a
/// retuned duration. `always` means until the app is quit or the switch is turned
/// off; the Live Activity that shows it lasts at most eight hours per app visit,
/// which the Settings footer says.
enum StandbyTimeout: String, CaseIterable, Identifiable {
    case off
    case fiveMinutes = "5m"
    case fifteenMinutes = "15m"
    case oneHour = "60m"
    case always

    var id: String { rawValue }

    var label: String {
        switch self {
        case .off: return "Off"
        case .fiveMinutes: return "5 minutes"
        case .fifteenMinutes: return "15 minutes"
        case .oneHour: return "1 hour"
        case .always: return "Always"
        }
    }

    /// Seconds of idleness before the mic is released; nil for no limit.
    var seconds: TimeInterval? {
        switch self {
        case .off, .always: return nil
        case .fiveMinutes: return 5 * 60
        case .fifteenMinutes: return 15 * 60
        case .oneHour: return 60 * 60
        }
    }
}

extension CleanupLevel {
    /// What the keyboard's pill and the app's picker call each level. One place, so
    /// the two surfaces agree.
    var label: String {
        switch self {
        case .none: return "Off"
        case .messaging: return "Messaging"
        case .light: return "Light"
        }
    }

    /// The order the pill cycles in: the two real registers first, off last.
    var next: CleanupLevel {
        switch self {
        case .light: return .messaging
        case .messaging: return .none
        case .none: return .light
        }
    }
}

/// The API keys, in the Keychain and nowhere else.
///
/// Never in `UserDefaults`, never in the App Group, never in a file — and the
/// keyboard extension never needs them, because the app makes every network call.
/// That is not only tidier: a keyboard is the one process on the device that sees
/// every keystroke, and it has no business holding a credential.
///
/// One Keychain item holds every key, one per line, the same layout the Mac uses.
/// A key stored before there was a list is a one-line item and reads as a list of one.
enum APIKey {
    private static let service = "com.whimpr.whimprflow"
    private static let account = "groq.api.key"

    /// Every stored key, in the order they were added — which is the order they are
    /// tried in.
    static func loadAll() -> [String] {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &item) == errSecSuccess,
              let data = item as? Data,
              let text = String(data: data, encoding: .utf8)
        else { return [] }
        var keys: [String] = []
        for line in text.split(separator: "\n") {
            let key = line.trimmingCharacters(in: .whitespacesAndNewlines)
            if !key.isEmpty && !keys.contains(key) { keys.append(key) }
        }
        return keys
    }

    /// Append a key. Adding one already stored changes nothing.
    static func add(_ key: String) -> Result<Void, SaveError> {
        let trimmed = key.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .success(()) }
        var keys = loadAll()
        guard !keys.contains(trimmed) else { return .success(()) }
        keys.append(trimmed)
        return saveAll(keys)
    }

    static func remove(at index: Int) -> Result<Void, SaveError> {
        var keys = loadAll()
        guard keys.indices.contains(index) else { return .success(()) }
        keys.remove(at: index)
        return saveAll(keys)
    }

    /// The ends of a key, for a settings list: enough to tell two apart, never enough
    /// to use. Display only, so it lives here rather than crossing the bridge.
    static func mask(_ key: String) -> String {
        guard key.count >= 12 else { return "••••" }
        return "\(key.prefix(4))…\(key.suffix(4))"
    }

    /// Why a save did not happen. Surfaced rather than swallowed: a silently failed
    /// save leaves the UI insisting there is no key while the user is sure they just
    /// entered one, with nothing to go on.
    enum SaveError: LocalizedError {
        /// The Keychain refused for want of an `application-identifier` entitlement,
        /// which means the build is unsigned. Expected in an unsigned simulator
        /// build; on a device it means provisioning is wrong.
        case missingEntitlement
        case keychain(OSStatus)

        var errorDescription: String? {
            switch self {
            case .missingEntitlement:
                return "the Keychain is unavailable in an unsigned build"
            case let .keychain(status):
                return "Keychain error \(status)"
            }
        }
    }

    /// Store the whole list. An empty list removes the item.
    static func saveAll(_ keys: [String]) -> Result<Void, SaveError> {
        let base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
        // Delete first rather than SecItemUpdate: an update against a missing item
        // fails, and branching on that is more code than simply replacing.
        SecItemDelete(base as CFDictionary)
        let text = keys.joined(separator: "\n")
        guard !text.isEmpty else { return .success(()) }

        var add = base
        add[kSecValueData as String] = Data(text.utf8)
        // The key is needed while dictating, which can happen from a locked-screen
        // keyboard; `AfterFirstUnlock` is the loosest setting that still keeps it
        // out of a backup restored onto another device.
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly

        let status = SecItemAdd(add as CFDictionary, nil)
        switch status {
        case errSecSuccess: return .success(())
        // -34018. The build carries no entitlements at all, so there is no keychain
        // access group to write into.
        case errSecMissingEntitlement: return .failure(.missingEntitlement)
        default: return .failure(.keychain(status))
        }
    }

    static var isSet: Bool { !loadAll().isEmpty }
}
