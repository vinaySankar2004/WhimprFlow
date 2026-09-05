import SwiftUI

/// The Deep-Slate / Aqua-Whimpr palette, ported from `ui/src/tokens/values.ts`.
///
/// Kept in the same order and under the same names as the TypeScript, so the two can
/// be diffed by eye. If you retune a colour, retune it there too — this is a port,
/// not a second opinion.
enum Theme {
    // Cool near-black slate scale (the pill hue).
    static let slate950 = Color(hex: 0x0C0E12)
    static let slate900 = Color(hex: 0x111419)
    static let slate850 = Color(hex: 0x161A20)
    static let slate800 = Color(hex: 0x1C212A)
    static let slate700 = Color(hex: 0x28303B)
    static let slate600 = Color(hex: 0x3A4453)
    static let slate500 = Color(hex: 0x5A6675)
    static let slate400 = Color(hex: 0x8A93A3)
    static let slate300 = Color(hex: 0xB8C0CC)
    static let slate200 = Color(hex: 0xD9DEE6)
    static let slate100 = Color(hex: 0xEDF0F4)

    // Cyan/teal accent.
    static let accent400 = Color(hex: 0x3FE0D0)
    static let accent500 = Color(hex: 0x22C3B6)
    static let accent600 = Color(hex: 0x12A99D)
    static let accentGlow = Color(red: 58 / 255, green: 232 / 255, blue: 216 / 255, opacity: 0.45)

    // Pale mint pill text + waveform bars.
    static let pillText = Color(hex: 0xDAF3EA)
    static let pillTextMuted = Color(hex: 0x8FB6AD)
    static let waveBar = Color(hex: 0xCFF3EA)

    // Semantic.
    static let error = Color(hex: 0xFF6B6B)
    static let warn = Color(hex: 0xF5B454)
    static let info = Color(hex: 0x5AA9FF)
    static let success = Color(hex: 0x22C3B6)

    /// The status ring's gradient stops (teal → aqua → periwinkle → violet → aqua).
    static let ringStops = [
        Color(hex: 0x1FB6A8), Color(hex: 0x43E6D6), Color(hex: 0x57B0FF),
        Color(hex: 0x9E86FF), Color(hex: 0x43E6D6),
    ]

    /// Motion, from the same token file. `spring` is the shared feel for state
    /// changes; anything slower reads as lag on a gesture this short.
    static let spring = Animation.spring(response: 0.28, dampingFraction: 0.72)

    // MARK: - Surfaces

    /// The app background. A vertical wash rather than a flat fill so the recording
    /// glow has something to sit against.
    ///
    /// Typed as `LinearGradient` and not `some ShapeStyle`: the opaque type erases
    /// the `View` conformance, and this is used both ways — as a fill and as a layer
    /// that ignores the safe area.
    static var background: LinearGradient {
        LinearGradient(
            colors: [slate950, slate900],
            startPoint: .top,
            endPoint: .bottom
        )
    }

    /// A raised card, matching the overlay pill's fill and hairline border.
    static let cardFill = slate850
    static let cardBorder = Color.white.opacity(0.06)
}

extension Color {
    /// Build from the `0xRRGGBB` literals the token file uses, so the two read alike.
    init(hex: UInt32) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255,
            opacity: 1
        )
    }
}

// MARK: - Shared building blocks

/// A raised surface with the pill's fill, radius and hairline border.
struct Card<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        content
            .padding(18)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.cardFill, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 20, style: .continuous)
                    .strokeBorder(Theme.cardBorder, lineWidth: 1)
            )
    }
}

/// Section heading in the app's own voice — small, spaced, muted.
struct SectionLabel: View {
    let text: String

    init(_ text: String) { self.text = text }

    var body: some View {
        Text(text.uppercased())
            .font(.caption.weight(.semibold))
            .tracking(1.1)
            .foregroundStyle(Theme.slate400)
    }
}
