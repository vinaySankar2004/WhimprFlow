import SwiftUI

/// The dictation screen: one big control, and enough feedback to know what it is
/// doing without reading anything.
struct DictateView: View {
    @Environment(DictationController.self) private var dictation
    @State private var settings = Settings.shared

    var body: some View {
        ZStack {
            Theme.background.ignoresSafeArea()

            // The control, its status and whatever it has to say are one centred
            // group; only the level picker is pinned to the bottom. Letting the
            // message drift to the bottom instead leaves a screen-height gap on iPad
            // between the thing you tapped and the thing it told you.
            VStack(spacing: 0) {
                Spacer(minLength: 0)

                VStack(spacing: 24) {
                    MicButton(
                        phase: dictation.phase,
                        level: dictation.level,
                        // Disabled rather than tappable-then-failing. The notice
                        // below already says exactly what is missing, and answering
                        // a tap with "Something went wrong" turns a clear
                        // configuration message into a vague error.
                        isBlocked: dictation.blocker != nil,
                        action: { dictation.toggle() }
                    )

                    StatusLine(phase: dictation.phase, isBlocked: dictation.blocker != nil)
                        .contentColumn(maxWidth: 420)

                    message
                        .contentColumn()
                        .padding(.horizontal, 20)
                }

                Spacer(minLength: 0)

                LevelPicker(level: $settings.level)
                    .contentColumn()
                    .padding(.horizontal, 20)
                    .padding(.bottom, 12)
            }
        }
        .animation(Theme.spring, value: dictation.phase)
        .navigationTitle("WhimprFlow")
        .navigationBarTitleDisplayMode(.inline)
        .toolbarBackground(Theme.background, for: .navigationBar)
    }

    /// Whatever the screen currently has to say: why it cannot start, what it
    /// produced, or what went wrong. At most one at a time, and nothing when idle.
    @ViewBuilder
    private var message: some View {
        if let blocker = dictation.blocker {
            Notice(text: blocker, tone: .warning)
        } else {
            switch dictation.phase {
            case let .done(text):
                ResultCard(
                    text: text,
                    engine: dictation.lastEngine,
                    degraded: dictation.lastDegraded
                )
                .transition(.scale(scale: 0.96).combined(with: .opacity))
            case let .failed(reason):
                Notice(text: reason, tone: .error)
                    .transition(.opacity)
            default:
                EmptyView()
            }
        }
    }
}

// MARK: - The button

/// The mic control, and the app's only real piece of motion.
///
/// Three distinct states have to be legible at a glance and from across a room:
/// idle breathes slowly, recording pulses rings sized by the live mic level, and
/// transcribing sweeps the status ring. Using one shared animation for all three
/// made the states indistinguishable, which is worse than no animation at all.
struct MicButton: View {
    let phase: DictationController.Phase
    let level: Float
    var isBlocked = false
    let action: () -> Void

    @State private var breathe = false
    @State private var sweep = false

    private var isRecording: Bool { phase == .recording }
    private var isTranscribing: Bool { phase == .transcribing }

    var body: some View {
        Button(action: action) {
            ZStack {
                // Level-driven rings. Two, offset, so a steady voice still reads as
                // movement rather than a single ring parked at one size.
                if isRecording {
                    ForEach(0..<2) { index in
                        Circle()
                            .stroke(Theme.accent.opacity(0.28 - Double(index) * 0.1), lineWidth: 2)
                            .frame(width: ringSize(index))
                            .animation(.easeOut(duration: 0.18), value: level)
                    }
                }

                // The sweeping gradient while the network is working.
                if isTranscribing {
                    Circle()
                        .strokeBorder(
                            AngularGradient(colors: Theme.ringStops, center: .center),
                            lineWidth: 3
                        )
                        .frame(width: 132)
                        .rotationEffect(.degrees(sweep ? 360 : 0))
                        .animation(.linear(duration: 1.4).repeatForever(autoreverses: false), value: sweep)
                }

                Circle()
                    .fill(fill)
                    .frame(width: 116)
                    .overlay(Circle().strokeBorder(Theme.border, lineWidth: 1))
                    .shadow(color: isRecording ? Theme.accentGlow : .black.opacity(0.5),
                            radius: isRecording ? 26 : 14, y: 8)
                    .scaleEffect(breathe && !isRecording && !isTranscribing ? 1.03 : 1.0)

                if isRecording {
                    Waveform(level: level)
                        .frame(width: 56, height: 30)
                } else {
                    Image(systemName: icon)
                        .font(.system(size: 40, weight: .medium))
                        .foregroundStyle(isTranscribing ? Theme.textSecondary : Theme.textPrimary)
                        .contentTransition(.symbolEffect(.replace))
                }
            }
            .frame(width: 180, height: 180)
        }
        .buttonStyle(.plain)
        .disabled(isTranscribing || isBlocked)
        .opacity(isBlocked ? 0.45 : 1)
        .accessibilityLabel(accessibilityLabel)
        .accessibilityHint(isBlocked ? "Not yet configured" : "")
        .accessibilityAddTraits(.isButton)
        .onAppear {
            withAnimation(.easeInOut(duration: 2.4).repeatForever(autoreverses: true)) {
                breathe = true
            }
            sweep = true
        }
    }

    /// Rings grow with the voice, from a resting size so silence still shows the
    /// control is live.
    private func ringSize(_ index: Int) -> CGFloat {
        let base: CGFloat = 132 + CGFloat(index) * 22
        return base + CGFloat(level) * (34 - CGFloat(index) * 8)
    }

    private var fill: Color { isRecording ? Theme.control : Theme.surface }

    private var icon: String {
        switch phase {
        case .transcribing: return "waveform"
        case .done: return "checkmark"
        case .failed: return "exclamationmark.triangle"
        default: return "mic.fill"
        }
    }

    private var accessibilityLabel: String {
        switch phase {
        case .recording: return "Stop recording"
        case .transcribing: return "Transcribing"
        default: return "Start dictating"
        }
    }
}

/// Five bars that track the live level, tallest in the middle.
///
/// The same shape as the Mac overlay's waveform, at the size that fits inside the
/// button rather than across a pill.
struct Waveform: View {
    let level: Float

    private let bars = 5

    var body: some View {
        HStack(alignment: .center, spacing: 4) {
            ForEach(0..<bars, id: \.self) { index in
                Capsule()
                    .fill(Theme.waveBar)
                    .frame(width: 5, height: height(index))
                    .animation(
                        .spring(response: 0.22, dampingFraction: 0.6)
                            .delay(Double(index) * 0.02),
                        value: level
                    )
            }
        }
    }

    private func height(_ index: Int) -> CGFloat {
        // Centre bar full height, falling off symmetrically to the edges.
        let distance = abs(Double(index) - Double(bars - 1) / 2)
        let falloff = 1.0 - (distance / Double(bars))
        let minimum: CGFloat = 6
        let maximum: CGFloat = 30
        return minimum + (maximum - minimum) * CGFloat(Double(level) * falloff)
    }
}

// MARK: - Supporting views

struct StatusLine: View {
    let phase: DictationController.Phase
    var isBlocked = false

    var body: some View {
        Text(isBlocked ? "Not set up yet" : text)
            .font(.subheadline)
            .foregroundStyle(Theme.textSecondary)
            .multilineTextAlignment(.center)
            .frame(minHeight: 22)
    }

    private var text: String {
        switch phase {
        case .idle: return "Tap to dictate"
        case .recording: return "Listening — tap to finish"
        case .transcribing: return "Transcribing and cleaning up"
        case .done: return "Copied to the clipboard"
        case .failed: return "Something went wrong"
        }
    }
}

struct ResultCard: View {
    let text: String
    let engine: Engine?
    let degraded: String?

    var body: some View {
        Card {
            VStack(alignment: .leading, spacing: 12) {
                SectionLabel("Result")
                Text(text)
                    .font(.body)
                    .foregroundStyle(Theme.textPrimary)
                    .textSelection(.enabled)
                // Every fallback in this app is deliberately silent, so a raw or slow
                // result has no explanation unless it is shown. This is that.
                if let degraded {
                    Text(degraded)
                        .font(.caption.monospaced())
                        .foregroundStyle(Theme.warn)
                } else if let engine, engine == .raw {
                    Text("pasted raw — cleanup was rejected by the gates")
                        .font(.caption)
                        .foregroundStyle(Theme.textSecondary)
                }
            }
        }
    }
}

struct Notice: View {
    enum Tone { case warning, error }

    let text: String
    let tone: Tone

    var body: some View {
        Card {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: tone == .error ? "exclamationmark.triangle.fill" : "info.circle.fill")
                    .foregroundStyle(tone == .error ? Theme.error : Theme.warn)
                Text(text)
                    .font(.subheadline)
                    .foregroundStyle(Theme.textPrimary)
            }
        }
    }
}

/// The cleanup level, inline on the main screen because it is the one setting people
/// change mid-conversation — chat register for messages, light for everything else.
struct LevelPicker: View {
    @Binding var level: CleanupLevel

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            SectionLabel("Cleanup")
            Picker("Cleanup level", selection: $level) {
                Text("Off").tag(CleanupLevel.none)
                Text("Light").tag(CleanupLevel.light)
                Text("Messaging").tag(CleanupLevel.messaging)
            }
            .pickerStyle(.segmented)
        }
    }
}
