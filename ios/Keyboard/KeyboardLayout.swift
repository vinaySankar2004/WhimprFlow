import UIKit

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
    /// iPad only, as on the stock keyboard there.
    case tab
    /// iPad only: caps lock, one tap on or off, at the start of the home row.
    case capsLock
    /// iPad only: dismiss the keyboard.
    case hide
    /// iPad only: the same as the bar's mic, where the stock keyboard puts its own.
    case dictate

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
            case .numbers: return UIDevice.current.userInterfaceIdiom == .pad ? ".?123" : "123"
            case .symbols: return "#+="
            }
        default: return nil
        }
    }
}

enum KeyboardLayout {
    /// `numberRow` puts 1234567890 above the letters, as the stock iPad keyboard
    /// does; the phone has no height to spare for it.
    static func rows(for plane: Plane, includeGlobe: Bool, numberRow: Bool = false) -> [[Key]] {
        let bottom: [Key] = includeGlobe
            ? [bottomPlaneKey(plane), .globe, .space, .return]
            : [bottomPlaneKey(plane), .space, .return]

        switch plane {
        case .letters:
            return (numberRow ? [chars("1234567890")] : []) + [
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

    // MARK: - iPad

    /// The stock iPad arrangement: tab and delete flank the top row, return ends
    /// the home row, shift sits at both ends of the third, and the bottom row is
    /// globe · .?123 · mic · space · .?123 · hide. Numbers and symbols are not a
    /// row of their own but secondary labels on the letters (`padSecondary`),
    /// typed with a downward flick — which is why the iPad grid has four rows.
    static func padRows(for plane: Plane, includeGlobe: Bool) -> [[Key]] {
        let leave = bottomPlaneKey(plane)
        var bottom: [Key] = includeGlobe ? [.globe] : []
        bottom += [leave, .dictate, .space, leave, .hide]
        switch plane {
        case .letters:
            return [
                [.tab] + chars("qwertyuiop") + [.delete],
                [.capsLock] + chars("asdfghjkl") + [.return],
                [.shift] + chars("zxcvbnm,.") + [.shift],
                bottom,
            ]
        case .numbers:
            return [
                chars("1234567890") + [.delete],
                [.plane(.symbols)] + chars("-/:;()$&@") + [.return],
                [.plane(.symbols)] + chars(".,?!'\"%+=") + [.plane(.symbols)],
                bottom,
            ]
        case .symbols:
            return [
                chars("[]{}#%^*+=") + [.delete],
                [.plane(.numbers)] + chars("_\\|~<>€£¥") + [.return],
                [.plane(.numbers)] + chars(".,?!'•©®™") + [.plane(.numbers)],
                bottom,
            ]
        }
    }

    /// What a downward flick on an iPad letter key types, as printed small at the
    /// top of the key. The stock layout's own assignments.
    static let padSecondary: [String: String] = {
        var map: [String: String] = [:]
        for (letter, secondary) in zip("qwertyuiop", "1234567890") { map[String(letter)] = String(secondary) }
        for (letter, secondary) in zip("asdfghjkl", "@#$&*()'\"") { map[String(letter)] = String(secondary) }
        for (letter, secondary) in zip("zxcvbnm,.", "%-+=/;:!?") { map[String(letter)] = String(secondary) }
        return map
    }()

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
