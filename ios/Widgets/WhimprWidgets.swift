import ActivityKit
import SwiftUI
import WidgetKit

@main
struct WhimprWidgetsBundle: WidgetBundle {
    var body: some Widget {
        StandbyActivityWidget()
    }
}

/// The standby session in the Dynamic Island and on the Lock Screen.
///
/// The compact trailing slot is left empty on purpose: iOS places its own orange
/// microphone indicator there when an activity is showing, so the island reads
/// "WhimprFlow · mic" with nothing added and nothing hidden.
struct StandbyActivityWidget: Widget {
    var body: some WidgetConfiguration {
        ActivityConfiguration(for: StandbyActivityAttributes.self) { context in
            LockScreenView(state: context.state)
                .activityBackgroundTint(Color(uiColor: Palette.slate900))
                .activitySystemActionForegroundColor(Color(uiColor: Palette.accent400))
        } dynamicIsland: { context in
            DynamicIsland {
                DynamicIslandExpandedRegion(.leading) {
                    RingGlyph(size: 28)
                        .padding(.leading, 4)
                        .padding(.top, 6)
                }
                DynamicIslandExpandedRegion(.center) {
                    StatusLines(state: context.state)
                }
                DynamicIslandExpandedRegion(.bottom) {
                    ActionRow(state: context.state)
                        .padding(.top, 4)
                }
            } compactLeading: {
                // Small, and kept clear of the sensor housing on its right: at 18 pt
                // the ring's edge disappeared under the cutout on an iPhone 17 Pro.
                RingGlyph(size: 16)
                    .padding(.leading, 2)
                    .padding(.trailing, 2)
            } compactTrailing: {
                // Deliberately empty. See the type comment.
                EmptyView()
            } minimal: {
                RingGlyph(size: 15)
            }
            .keylineTint(Color(uiColor: Palette.accent400))
        }
    }
}

// MARK: - Pieces

/// The app icon's mark — concentric rings — drawn rather than shipped as an image, so
/// it stays crisp at every island size and tints with the palette.
struct RingGlyph: View {
    let size: CGFloat

    var body: some View {
        // The stroke is drawn centred on the circle's edge, so the ring's diameter
        // is the frame less one line width — otherwise the stroke overflows the
        // frame on every side, and in the island's compact slot the overflow is
        // clipped, which read as the glyph being "too big".
        let line = size * 0.18
        ZStack {
            Circle()
                .stroke(Color(uiColor: Palette.accent400), lineWidth: line)
                .frame(width: size - line, height: size - line)
            Circle()
                .fill(Color(uiColor: Palette.accent400))
                .frame(width: size * 0.34, height: size * 0.34)
        }
        .frame(width: size, height: size)
        .accessibilityLabel("WhimprFlow")
    }
}

struct StatusLines: View {
    let state: StandbyActivityAttributes.ContentState
    /// Centred in the island's expanded view, leading on the Lock Screen banner —
    /// the two places Apple's own activities align differently.
    var alignment: HorizontalAlignment = .center

    var body: some View {
        VStack(alignment: alignment, spacing: 2) {
            Text(title)
                .font(.headline)
                .foregroundStyle(.white)
            subtitle
                .font(.caption)
                .foregroundStyle(Color(uiColor: Palette.slate300))
                .monospacedDigit()
        }
        .multilineTextAlignment(alignment == .leading ? .leading : .center)
    }

    private var title: String {
        switch state.phase {
        case .ready: return "Mic ready"
        case .listening: return "Listening"
        case .transcribing: return "Transcribing…"
        }
    }

    @ViewBuilder
    private var subtitle: some View {
        switch state.phase {
        case .listening:
            if let startedAt = state.startedAt {
                HStack(spacing: 4) {
                    if let input = state.inputName { Text(input) ; Text("·") }
                    // A timer Text grows to fill its row unless capped; the cap must
                    // still fit "12:34" on the Lock Screen, where the font is larger
                    // than in the island — 48 pt squeezed it into "1:––".
                    Text(timerInterval: startedAt...startedAt.addingTimeInterval(20 * 60), countsDown: false)
                        .frame(maxWidth: 64, alignment: .leading)
                }
            } else if let input = state.inputName {
                Text(input)
            }
        case .transcribing:
            Text("Cleaning up what you said")
        case .ready:
            if let releaseAt = state.releaseAt {
                HStack(spacing: 4) {
                    Text("Releases in")
                    Text(timerInterval: Date()...releaseAt, countsDown: true)
                        .frame(maxWidth: 64, alignment: .leading)
                }
            } else {
                Text("Tap the keyboard's mic to dictate")
            }
        }
    }
}

struct ActionRow: View {
    let state: StandbyActivityAttributes.ContentState
    var alignment: Alignment = .center

    var body: some View {
        HStack(spacing: 10) {
            switch state.phase {
            case .listening:
                Button(intent: DiscardDictationIntent()) {
                    Label("Discard", systemImage: "xmark")
                }
                .tint(Color(uiColor: Palette.slate600))
                Button(intent: StopDictationIntent()) {
                    Label("Finish", systemImage: "checkmark")
                }
                .tint(Color(uiColor: Palette.accent500))
            case .transcribing:
                Label("Inserting into your keyboard", systemImage: "text.cursor")
                    .font(.caption)
                    .foregroundStyle(Color(uiColor: Palette.slate300))
            case .ready:
                Button(intent: ReleaseMicIntent()) {
                    Label("Release mic", systemImage: "mic.slash")
                }
                .tint(Color(uiColor: Palette.slate600))
            }
        }
        .buttonStyle(.borderedProminent)
        .buttonBorderShape(.capsule)
        .controlSize(.small)
        .font(.caption.weight(.semibold))
        .frame(maxWidth: .infinity, alignment: alignment)
    }
}

/// The Lock Screen banner: icon at the leading edge, text and the action beside it,
/// everything leading-aligned — the shape of every system activity there.
struct LockScreenView: View {
    let state: StandbyActivityAttributes.ContentState

    var body: some View {
        HStack(alignment: .center, spacing: 14) {
            RingGlyph(size: 34)
            VStack(alignment: .leading, spacing: 10) {
                StatusLines(state: state, alignment: .leading)
                ActionRow(state: state, alignment: .leading)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 14)
    }
}
