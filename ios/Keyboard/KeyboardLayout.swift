import Foundation

/// The three planes of the keyboard, as data.
///
/// Rows are arrays of keys; the geometry (how wide a shift key is, where the second
/// row's half-key inset goes) lives in `KeyboardView`, which knows the width it has.
/// Keeping the planes here means a layout question — "what is on the symbols plane" —
/// has one answer, readable without running anything.
///
/// The arrangement is the stock iPhone keyboard's, which is also Wispr Flow's: people
/// type on it by feel, and a key that is not where their thumb expects is a typo they
/// blame on us.
enum Plane {
    case letters, numbers, symbols
}

enum Key: Hashable {
    /// Inserts its string. On the letters plane the string is lowercase and shift
    /// decides the case at insertion time.
    case character(String)
    case shift
    case delete
    case space
    case `return`
    /// Only present when iOS is not drawing its own globe below the keyboard — see
    /// `needsInputModeSwitchKey`.
    case globe
    /// Switch plane. The title is what the stock keyboard prints: "123", "ABC", "#+=".
    case plane(Plane)

    /// Whether this is a modifier — drawn in the darker key colour.
    var isModifier: Bool {
        switch self {
        case .character, .space: return false
        default: return true
        }
    }

    /// The title, for keys that have one. `nil` for keys drawn with a symbol.
    var title: String? {
        switch self {
        case let .character(text): return text
        case let .plane(plane):
            switch plane {
            case .letters: return "ABC"
            case .numbers: return "123"
            case .symbols: return "#+="
            }
        default: return nil
        }
    }
}

enum KeyboardLayout {
    static func rows(for plane: Plane, includeGlobe: Bool) -> [[Key]] {
        let bottom: [Key] = includeGlobe
            ? [bottomPlaneKey(plane), .globe, .space, .return]
            : [bottomPlaneKey(plane), .space, .return]

        switch plane {
        case .letters:
            return [
                chars("qwertyuiop"),
                chars("asdfghjkl"),
                [.shift] + chars("zxcvbnm") + [.delete],
                bottom,
            ]
        case .numbers:
            return [
                chars("1234567890"),
                chars("-/:;()$&@\""),
                [.plane(.symbols)] + chars(".,?!'") + [.delete],
                bottom,
            ]
        case .symbols:
            return [
                chars("[]{}#%^*+="),
                chars("_\\|~<>€£¥•"),
                [.plane(.numbers)] + chars(".,?!'") + [.delete],
                bottom,
            ]
        }
    }

    /// The bottom-left key leaves the plane: letters → numbers, and both of the
    /// others → letters, as on the stock keyboard.
    private static func bottomPlaneKey(_ plane: Plane) -> Key {
        plane == .letters ? .plane(.numbers) : .plane(.letters)
    }

    private static func chars(_ text: String) -> [Key] {
        text.map { .character(String($0)) }
    }
}

/// Shift, in its three states. `locked` is caps lock, reached by a double tap.
enum ShiftState {
    case off, on, locked

    var isActive: Bool { self != .off }
}
