import SwiftUI

/// Settings: the key, the keyboard, the dictionary, and the one switch that trades
/// reliability for speed.
struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var settings = Settings.shared

    @State private var keyDraft = ""
    @State private var keyIsSet = APIKey.isSet
    @State private var saveError: String?
    @State private var check: ConnectionCheck = .idle

    /// The result of asking Groq whether the key actually works.
    ///
    /// Worth its own control because every other symptom of a bad key is indirect:
    /// dictation simply falls back to raw, with the reason buried in a caption.
    enum ConnectionCheck: Equatable {
        case idle, running, ok(String), failed(String)
    }

    var body: some View {
        NavigationStack {
            Form {
                keySection
                keyboardSection
                dictionarySection
                behaviourSection
                aboutSection
            }
            .scrollContentBackground(.hidden)
            .background(Theme.background.ignoresSafeArea())
            .navigationTitle("Settings")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .tint(Theme.accent400)
        .preferredColorScheme(.dark)
    }

    // MARK: - Sections

    private var keySection: some View {
        Section {
            SecureField(keyIsSet ? "Replace the stored key" : "gsk_…", text: $keyDraft)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .font(.body.monospaced())

            HStack {
                Button(keyIsSet ? "Replace key" : "Save key") { saveKey() }
                    .disabled(keyDraft.trimmingCharacters(in: .whitespaces).isEmpty)

                Spacer()

                if keyIsSet {
                    Button("Remove", role: .destructive) {
                        _ = APIKey.save("")
                        keyIsSet = APIKey.isSet
                        saveError = nil
                        check = .idle
                    }
                }
            }

            if let saveError {
                Label(saveError, systemImage: "exclamationmark.triangle.fill")
                    .font(.caption)
                    .foregroundStyle(Theme.error)
            }

            Button {
                Task { await runCheck() }
            } label: {
                HStack {
                    Text("Check connection")
                    Spacer()
                    switch check {
                    case .idle: EmptyView()
                    case .running: ProgressView()
                    case let .ok(detail):
                        Label(detail, systemImage: "checkmark.circle.fill")
                            .foregroundStyle(Theme.success)
                            .font(.caption)
                    case let .failed(detail):
                        Label(detail, systemImage: "xmark.circle.fill")
                            .foregroundStyle(Theme.error)
                            .font(.caption)
                    }
                }
            }
            .disabled(!keyIsSet || check == .running)
        } header: {
            Text("Groq API key")
        } footer: {
            Text("Stored in the iOS Keychain, never in a file and never in the shared container. The keyboard extension never sees it — the app makes every network call.")
        }
    }

    private var keyboardSection: some View {
        Section {
            Link(destination: URL(string: UIApplication.openSettingsURLString)!) {
                Label("Open iOS Settings", systemImage: "arrow.up.forward.app")
            }
        } header: {
            Text("Keyboard")
        } footer: {
            // Stated plainly because both steps are non-obvious and the failure mode
            // of skipping the second is a mic key that silently does nothing.
            Text("""
            To dictate inside other apps, add the WhimprFlow keyboard in Settings → General → Keyboard → Keyboards → Add New Keyboard, then tap it and turn on Allow Full Access.

            Full Access is required: without it the keyboard has no network and cannot see the app's results. A keyboard extension cannot record audio at all — iOS does not permit it — so the mic key hands recording to this app.
            """)
        }
    }

    private var dictionarySection: some View {
        Section {
            NavigationLink {
                DictionaryView()
            } label: {
                LabeledContent("Dictionary") {
                    Text("\(settings.dictionary.entries.count)")
                        .foregroundStyle(Theme.slate400)
                }
            }
        } footer: {
            Text("Names and terms recognition gets wrong, and the spellings to use instead.")
        }
    }

    private var behaviourSection: some View {
        Section {
            Toggle("Keep the mic ready in the background", isOn: $settings.keepSessionAlive)
        } header: {
            Text("Behaviour")
        } footer: {
            Text("""
            On, the mic key records without visibly switching to this app — but iOS may suspend the app, and dictation then falls back to opening it.

            Off, the mic key always opens this app first. Slower, and completely reliable.
            """)
        }
    }

    private var aboutSection: some View {
        Section {
            LabeledContent("Recognition", value: Settings.Groq.asrModel)
            LabeledContent("Cleanup", value: Settings.Groq.cleanupModel)
            LabeledContent("Core", value: bridgeVersion)
        } header: {
            Text("About")
        } footer: {
            Text("Audio and transcripts go to Groq. Nothing is stored off the device.")
        }
        .font(.caption.monospaced())
    }

    private var bridgeVersion: String {
        (try? "bridge \(WhimprCore.bridgeVersion())") ?? "unavailable"
    }

    // MARK: - Actions

    /// Save, and say so when it does not work.
    ///
    /// `keyIsSet` is re-read from the Keychain rather than assumed from a successful
    /// return, so the switch that shows "Replace key" can only ever reflect a key
    /// that is genuinely readable back.
    private func saveKey() {
        switch APIKey.save(keyDraft) {
        case .success:
            saveError = nil
            keyDraft = ""
        case let .failure(error):
            saveError = error.localizedDescription
        }
        keyIsSet = APIKey.isSet
        check = .idle
    }

    // MARK: - Connection check

    /// A real request, not a reachability ping: the useful question is whether *this
    /// key* is accepted by *this endpoint*, which nothing but a call can answer.
    private func runCheck() async {
        guard let key = APIKey.load() else {
            check = .failed("no key")
            return
        }
        check = .running
        do {
            let prepared = try WhimprCore.prepare(
                raw: "testing one two three",
                level: .light,
                dictionary: DictionaryStore()
            )
            _ = try await GroqClient(apiKey: key).cleanup(prepared: prepared)
            check = .ok("reachable")
        } catch {
            check = .failed(shortReason(error))
        }
    }

    private func shortReason(_ error: Error) -> String {
        if case let GroqClient.Failure.http(code, _) = error {
            switch code {
            case 401: return "key rejected"
            case 429: return "rate limited"
            default: return "HTTP \(code)"
            }
        }
        return "unreachable"
    }
}

/// The dictionary editor.
///
/// Entries are held in the core's own JSON shape, so what is edited here is exactly
/// what crosses the bridge — and exactly what could be copied to or from the Mac's
/// `dictionary.json`.
struct DictionaryView: View {
    @State private var settings = Settings.shared
    @State private var newCorrect = ""
    @State private var newMishears = ""

    var body: some View {
        Form {
            Section {
                TextField("Correct spelling", text: $newCorrect)
                    .autocorrectionDisabled()
                TextField("Mis-hearings, comma separated", text: $newMishears)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                Button("Add") { add() }
                    .disabled(newCorrect.trimmingCharacters(in: .whitespaces).isEmpty)
            } header: {
                Text("Add a word")
            } footer: {
                Text("For example, correct “Manvi” with mis-hearings “monvi, manvee”. Recognition is primed with these, and any that survive are enforced afterwards.")
            }

            Section("Words") {
                if settings.dictionary.entries.isEmpty {
                    Text("No words yet.").foregroundStyle(Theme.slate400)
                } else {
                    ForEach(Array(settings.dictionary.entries.enumerated()), id: \.offset) { index, entry in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(entry["correct"] as? String ?? "—")
                                .font(.body.weight(.medium))
                            let mishears = (entry["mishears"] as? [String]) ?? []
                            if !mishears.isEmpty {
                                Text(mishears.joined(separator: ", "))
                                    .font(.caption.monospaced())
                                    .foregroundStyle(Theme.slate400)
                            }
                        }
                        .swipeActions {
                            Button("Delete", role: .destructive) { remove(at: index) }
                        }
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .background(Theme.background.ignoresSafeArea())
        .navigationTitle("Dictionary")
        .navigationBarTitleDisplayMode(.inline)
    }

    private func add() {
        let correct = newCorrect.trimmingCharacters(in: .whitespaces)
        let mishears = newMishears
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { !$0.isEmpty }
        var entries = settings.dictionary.entries
        entries.removeAll { ($0["correct"] as? String)?.lowercased() == correct.lowercased() }
        entries.append(["correct": correct, "mishears": mishears, "source": "manual"])
        settings.dictionary = DictionaryStore(entries: entries)
        newCorrect = ""
        newMishears = ""
    }

    private func remove(at index: Int) {
        var entries = settings.dictionary.entries
        guard entries.indices.contains(index) else { return }
        entries.remove(at: index)
        settings.dictionary = DictionaryStore(entries: entries)
    }
}
