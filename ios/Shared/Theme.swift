import SwiftUI

/// SwiftUI's view of `Palette`.
///
/// A wrapper and not a second set of values: the keyboard is UIKit and the app is
/// SwiftUI, and a colour defined twice is a colour that will eventually differ in one
/// appearance and not the other. Everything here resolves per appearance because the
/// `UIColor`s underneath are dynamic.
enum Theme {
    // MARK: - Semantic

    static let background = Color(uiColor: Palette.background)
    static let surface = Color(uiColor: Palette.surface)
    static let control = Color(uiColor: Palette.control)
    static let border = Color(uiColor: Palette.border)

    static let textPrimary = Color(uiColor: Palette.textPrimary)
    static let textSecondary = Color(uiColor: Palette.textSecondary)

    static let accent = Color(uiColor: Palette.accent)
    static let waveBar = Color(uiColor: Palette.waveBar)

    static let error = Color(uiColor: Palette.error)
    static let warn = Color(uiColor: Palette.warn)
    static let success = Color(uiColor: Palette.success)

    /// The glow behind the mic while recording. Alpha-based so it reads as light
    /// spilling onto the background in either appearance rather than a grey halo.
    static let accentGlow = Color(uiColor: Palette.accent).opacity(0.45)

    /// The status ring's gradient stops (teal → aqua → periwinkle → violet → aqua).
    /// Fixed rather than per-appearance: it is a spinner, not a surface, and it sits
    /// on the mic button in both.
    static let ringStops = [
        Color(uiColor: UIColor(hex: 0x1FB6A8)),
        Color(uiColor: UIColor(hex: 0x43E6D6)),
        Color(uiColor: UIColor(hex: 0x57B0FF)),
        Color(uiColor: UIColor(hex: 0x9E86FF)),
        Color(uiColor: UIColor(hex: 0x43E6D6)),
    ]

    /// Motion, from `ui/src/tokens/values.ts`. The shared feel for state changes;
    /// anything slower reads as lag on a gesture this short.
    static let spring = Animation.spring(response: 0.28, dampingFraction: 0.72)
}

// MARK: - Shared building blocks

/// A raised surface with the pill's radius and hairline border.
struct Card<Content: View>: View {
    @ViewBuilder var content: Content

    var body: some View {
        content
            .padding(18)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(Theme.surface, in: RoundedRectangle(cornerRadius: 20, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 20, style: .continuous)
                    .strokeBorder(Theme.border, lineWidth: 1)
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
            .foregroundStyle(Theme.textSecondary)
    }
}
