import SwiftUI

@main
struct WhimprFlowApp: App {
    @Environment(\.scenePhase) private var scenePhase
    private let dictation = DictationController.shared
    /// Observed so a change in Settings re-renders the whole scene, which is what
    /// applies the appearance without a relaunch.
    @State private var settings = Settings.shared

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(dictation)
                .preferredColorScheme(settings.appearance.colorScheme)
                .task {
                    // A fresh process has no dictation in flight, whatever the last
                    // one left in the container — a `failed` or `recording` from a
                    // process that was killed would otherwise greet the keyboard with
                    // a stale screen.
                    Handoff.state = .idle
                    Handoff.captureStartedAt = nil
                    dictation.observeKeyboard()
                    _ = await Recorder.requestPermission()
                    dictation.refreshConfiguration()
                    dictation.startStandby()
                }
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            // Standby is entered on activation and deliberately *not* left on
            // backgrounding: the running capture engine is the only thing keeping the
            // app alive back there, and stopping it is what made every mic-key tap
            // fall back to opening the app. `.inactive` is transient and left alone.
            case .active:
                // The pill may have changed the level while the app was away, and a
                // foreground visit is when the island's activity can be (re)requested.
                settings.reloadLevel()
                dictation.startStandby()
                dictation.resumeActivityIfNeeded()
            case .background, .inactive: break
            @unknown default: break
            }
        }
    }
}

/// The app's one navigation container.
///
/// A plain `NavigationStack` rather than a split view: there are two screens, and a
/// sidebar on iPad for a single destination is a worse version of a toolbar button.
/// iPad is handled by constraining the content column instead — see `contentColumn`.
struct RootView: View {
    @Environment(DictationController.self) private var dictation
    @State private var showingSettings = false

    var body: some View {
        NavigationStack {
            DictateView()
                .toolbar {
                    ToolbarItem(placement: .topBarTrailing) {
                        Button {
                            showingSettings = true
                        } label: {
                            Image(systemName: "gearshape")
                        }
                        .accessibilityLabel("Settings")
                    }
                }
                // The key and the microphone permission both live outside the
                // observation graph, so the screen is told to re-read them whenever
                // they could have changed rather than polling.
                .sheet(isPresented: $showingSettings, onDismiss: {
                    dictation.refreshConfiguration()
                    // The standby switch and the API key both live in there, and each
                    // decides whether standby can run at all.
                    dictation.applyStandbyPreference()
                }) {
                    SettingsView()
                }
                .onAppear(perform: dictation.refreshConfiguration)
                // The keyboard's two ways in. `dictate` arrives when it could not
                // signal an already-running app and means "start recording now";
                // `settings` is its menu's Open WhimprFlow.
                .onOpenURL { url in
                    guard url.scheme == "whimprflow" else { return }
                    switch url.host {
                    case "dictate": dictation.startRecording()
                    case "settings": showingSettings = true
                    default: break
                    }
                }
        }
        .tint(Theme.accent)
    }
}

extension View {
    /// Constrain content to a readable column and centre it.
    ///
    /// The whole iPad story for this app. Without it every control stretches to a
    /// 1024-point width, which does not read as a bigger phone layout — it reads as
    /// a phone layout that was never looked at on iPad.
    func contentColumn(maxWidth: CGFloat = 520) -> some View {
        frame(maxWidth: maxWidth)
            .frame(maxWidth: .infinity)
    }
}
