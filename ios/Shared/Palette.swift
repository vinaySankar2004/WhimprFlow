import SwiftUI
import UIKit

/// The colours, in both appearances.
///
/// `UIColor` rather than SwiftUI's `Color` because the keyboard extension is UIKit and
/// needs the same palette; `Theme` wraps these for SwiftUI. One definition, so the two
/// halves of the app cannot drift.
///
/// The dark values are the Deep-Slate / Aqua-Whimpr scale from
/// `ui/src/tokens/values.ts`. The light values are new here — the Mac has no light
/// theme yet — and are chosen to keep the *relationships* rather than to invert the
/// numbers: the accent darkens (`accent600`) because a bright teal that reads well on
/// near-black is illegible on white, and the surfaces separate by lightness in the
/// opposite direction, cards sitting above the background rather than below it.
enum Palette {
    // MARK: - Raw scale

    static let slate950 = UIColor(hex: 0x0C0E12)
    static let slate900 = UIColor(hex: 0x111419)
    static let slate850 = UIColor(hex: 0x161A20)
    static let slate800 = UIColor(hex: 0x1C212A)
    static let slate700 = UIColor(hex: 0x28303B)
    static let slate600 = UIColor(hex: 0x3A4453)
    static let slate500 = UIColor(hex: 0x5A6675)
    static let slate400 = UIColor(hex: 0x8A93A3)
    static let slate300 = UIColor(hex: 0xB8C0CC)
    static let slate200 = UIColor(hex: 0xD9DEE6)
    static let slate100 = UIColor(hex: 0xEDF0F4)
    static let slate050 = UIColor(hex: 0xF7F9FB)

    static let accent400 = UIColor(hex: 0x3FE0D0)
    static let accent500 = UIColor(hex: 0x22C3B6)
    static let accent600 = UIColor(hex: 0x12A99D)

    // MARK: - Semantic

    /// The page behind everything.
    static let background = dynamic(light: .white, dark: slate950)

    /// A raised surface: cards, and the keyboard's own keys.
    static let surface = dynamic(light: slate050, dark: slate850)

    /// A key or control that should read as pressable against `surface`.
    static let control = dynamic(light: .white, dark: slate700)

    /// The hairline around a surface. Darkens the edge on light, lightens it on dark —
    /// a single translucent white is invisible on a white card.
    static let border = dynamic(
        light: UIColor.black.withAlphaComponent(0.10),
        dark: UIColor.white.withAlphaComponent(0.06)
    )

    static let textPrimary = dynamic(light: slate900, dark: slate100)
    static let textSecondary = dynamic(light: slate500, dark: slate400)

    /// Interactive tint. Darker on light so it clears contrast against white.
    static let accent = dynamic(light: accent600, dark: accent400)

    /// The waveform bars.
    static let waveBar = dynamic(light: accent600, dark: UIColor(hex: 0xCFF3EA))

    static let error = dynamic(light: UIColor(hex: 0xD8443C), dark: UIColor(hex: 0xFF6B6B))
    static let warn = dynamic(light: UIColor(hex: 0xB07300), dark: UIColor(hex: 0xF5B454))
    static let success = dynamic(light: accent600, dark: accent500)

    /// Build a colour that resolves per appearance.
    private static func dynamic(light: UIColor, dark: UIColor) -> UIColor {
        UIColor { traits in traits.userInterfaceStyle == .dark ? dark : light }
    }
}

extension UIColor {
    /// Built from the `0xRRGGBB` literals the token file uses, so the two read alike.
    convenience init(hex: UInt32) {
        self.init(
            red: CGFloat((hex >> 16) & 0xFF) / 255,
            green: CGFloat((hex >> 8) & 0xFF) / 255,
            blue: CGFloat(hex & 0xFF) / 255,
            alpha: 1
        )
    }
}

/// Which appearance the app uses, independent of the system.
///
/// Stored as a raw string so it round-trips through the shared container and can be
/// read by the keyboard, which has to match the app without being able to ask it.
enum Appearance: String, CaseIterable, Identifiable {
    case system, light, dark

    var id: String { rawValue }

    var label: String {
        switch self {
        case .system: return "System"
        case .light: return "Light"
        case .dark: return "Dark"
        }
    }

    /// `.unspecified` means "follow the system", which is what UIKit wants for the
    /// default rather than a resolved style — resolving it here would freeze the
    /// appearance at whatever it was when the view was built.
    var interfaceStyle: UIUserInterfaceStyle {
        switch self {
        case .system: return .unspecified
        case .light: return .light
        case .dark: return .dark
        }
    }

    /// SwiftUI's equivalent. `nil` is its spelling of "follow the system", and for the
    /// same reason: a resolved scheme would stop tracking the device.
    var colorScheme: ColorScheme? {
        switch self {
        case .system: return nil
        case .light: return .light
        case .dark: return .dark
        }
    }
}
