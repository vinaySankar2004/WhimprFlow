import SwiftUI

/// Settings: the key, the keyboard, the dictionary, and the one switch that trades
/// reliability for speed.
struct SettingsView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var settings = Settings.shared

    @State private var keyDraft = ""
    @State private var keys = APIKey.loadAll()
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
                appearanceSection
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
        .tint(Theme.accent)
        .preferredColorScheme(settings.appearance.colorScheme)
    }

    // MARK: - Sections

    private var keySection: some View {
        Section {
            ForEach(Array(keys.enumerated()), id: \.offset) { index, key in
                Text(APIKey.mask(key))
                    .font(.body.monospaced())
                    .swipeActions {
                        Button("Remove", role: .destructive) { removeKey(at: index) }
                    }
            }

            SecureField(keys.isEmpty ? "gsk_…" : "Add another key", text: $keyDraft)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .font(.body.monospaced())

            Button(keys.isEmpty ? "Save key" : "Add key") { saveKey() }
                .disabled(keyDraft.trimmingCharacters(in: .whitespaces).isEmpty)

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
                    case .ok:
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundStyle(Theme.success)
                    case .failed:
                        Image(systemName: "xmark.circle.fill")
                            .foregroundStyle(Theme.error)
                    }
                }
            }
            .disabled(keys.isEmpty || check == .running)

            // On its own line and in full. A refusal collapsed to "HTTP 403" tells
            // you a number; the sentence underneath it tells you whether to change
            // the key, the model, or nothing at all.
            switch check {
            case let .ok(detail):
                Text(detail).font(.caption).foregroundStyle(Theme.success)
            case let .failed(detail):
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(Theme.error)
                    .textSelection(.enabled)
            default:
                EmptyView()
            }
        } header: {
            Text("Groq API keys")
        } footer: {
            Text("Stored in the iOS Keychain, never in a file and never in the shared container. The keyboard extension never sees them — the app makes every network call. Add more than one and dictation moves to the next key when one is rate limited, then back when it frees up. Swipe a key to remove it.")
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
                        .foregroundStyle(Theme.textSecondary)
                }
            }
        } footer: {
            Text("Names and terms recognition gets wrong, and the spellings to use instead.")
        }
    }

    private var appearanceSection: some View {
        Section {
            Picker("Theme", selection: $settings.appearance) {
                ForEach(Appearance.allCases) { option in
                    Text(option.label).tag(option)
                }
            }
            .pickerStyle(.segmented)
        } header: {
            Text("Appearance")
        } footer: {
            Text("System follows your device's light or dark setting. The keyboard matches whatever you choose here.")
        }
    }

    private var behaviourSection: some View {
        Section {
            Toggle("Keep the mic ready in the background", isOn: $settings.keepSessionAlive)
        } header: {
            Text("Behaviour")
        } footer: {
            // The orange dot is the visible consequence of this switch, and someone
            // who finds it without being told will reasonably assume they are being
            // listened to. Say it here, before they turn it on.
            Text("""
            On, the mic key dictates without switching to this app. Keeping it ready means WhimprFlow stays running in the background, which uses some battery. The microphone is opened only while you dictate, so the orange microphone dot shows only then.

            Off, the mic key opens this app first. Slower, no background battery use.
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
    /// `keys` is re-read from the Keychain rather than assumed from a successful
    /// return, so the list can only ever show keys that are genuinely readable back.
    private func saveKey() {
        switch APIKey.add(keyDraft) {
        case .success:
            saveError = nil
            keyDraft = ""
        case let .failure(error):
            saveError = error.localizedDescription
        }
        keys = APIKey.loadAll()
        check = .idle
    }

    private func removeKey(at index: Int) {
        if case let .failure(error) = APIKey.remove(at: index) {
            saveError = error.localizedDescription
        } else {
            saveError = nil
        }
        keys = APIKey.loadAll()
        check = .idle
    }

    // MARK: - Connection check

    /// A real request, not a reachability ping: the useful question is whether *a
    /// stored key* is accepted by *this endpoint*, which nothing but a call can
    /// answer. Runs through the same ring dictation uses, so with several keys it
    /// checks the one dictation would send with next.
    private func runCheck() async {
        guard !keys.isEmpty else {
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
            _ = try await GroqClient(ring: KeyRing(keys: keys)).cleanup(prepared: prepared)
            check = .ok("Reachable — the key works and \(Settings.Groq.cleanupModel) answered.")
        } catch {
            // The error's own description, not a summary of it: these are the words
            // that say whether the key, the model or the account is the problem, and
            // a check that hides them is not a diagnostic.
            check = .failed(error.localizedDescription)
        }
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
                    Text("No words yet.").foregroundStyle(Theme.textSecondary)
                } else {
                    ForEach(Array(settings.dictionary.entries.enumerated()), id: \.offset) { index, entry in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(entry["correct"] as? String ?? "—")
                                .font(.body.weight(.medium))
                            let mishears = (entry["mishears"] as? [String]) ?? []
                            if !mishears.isEmpty {
                                Text(mishears.joined(separator: ", "))
                                    .font(.caption.monospaced())
                                    .foregroundStyle(Theme.textSecondary)
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
