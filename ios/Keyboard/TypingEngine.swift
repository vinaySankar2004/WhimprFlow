import UIKit

/// The typing behaviour that makes an uncorrected keyboard feel like the stock one.
///
/// A third-party keyboard gets no autocorrect and no predictions, and neither does
/// Wispr Flow's. What it can have is the rest of the muscle memory: a capital at the
/// start of a sentence, a full stop from a double space, caps lock from a double tap,
/// and the host field's own capitalisation and return-key preferences respected. Each
/// of those is a small rule; together they are the difference between a keyboard and
/// a grid of buttons.
///
/// This type decides; `KeyboardView` draws and `KeyboardViewController` connects.
final class TypingEngine {
    private let proxy: UITextDocumentProxy

    private(set) var shift: ShiftState = .off
    private(set) var plane: Plane = .letters

    /// When the last space was inserted, for the double-space full stop.
    private var lastSpaceAt: Date?
    /// When shift was last tapped, for caps lock.
    private var lastShiftAt: Date?

    private static let doubleTapWindow: TimeInterval = 0.4

    init(proxy: UITextDocumentProxy) {
        self.proxy = proxy
        refreshShift()
    }

    // MARK: - Keys

    /// Enact a committed key. Returns whether the plane or shift changed in a way the
    /// view has to redraw.
    @discardableResult
    func commit(_ key: Key) -> Bool {
        switch key {
        case let .character(text):
            insertCharacter(text)
            return settleAfterCharacter(text)
        case .space:
            insertSpace()
            return refreshShift()
        case .return:
            proxy.insertText("\n")
            return refreshShift()
        case .delete:
            proxy.deleteBackward()
            return refreshShift()
        case .shift:
            return toggleShift()
        case let .plane(next):
            plane = next
            // Coming back to letters, the sentence rule applies again; the other two
            // planes have no case.
            if next == .letters { refreshShift() }
            return true
        case .globe:
            return false
        }
    }

    private func insertCharacter(_ text: String) {
        // The double-space rule keys off *a* space, and typing anything else ends it.
        lastSpaceAt = nil
        let cased = shift.isActive && plane == .letters ? text.uppercased() : text
        proxy.insertText(cased)
    }

    /// One-shot shift drops after a letter; caps lock stays. Punctuation that ends a
    /// sentence arms shift for the next one, but only once a space follows — which
    /// `refreshShift` sees on the next key.
    private func settleAfterCharacter(_ text: String) -> Bool {
        if shift == .on, plane == .letters, text.first?.isLetter == true {
            shift = .off
            return true
        }
        return false
    }

    /// Two spaces within the window become ". " — the stock rule, and only when the
    /// character before the first space is a letter or digit, so "hi ..  " does not
    /// grow a stray full stop.
    private func insertSpace() {
        let now = Date()
        if let last = lastSpaceAt,
           now.timeIntervalSince(last) < Self.doubleTapWindow,
           let before = proxy.documentContextBeforeInput,
           before.hasSuffix(" "),
           let beforeSpace = before.dropLast().last,
           beforeSpace.isLetter || beforeSpace.isNumber {
            proxy.deleteBackward()
            proxy.insertText(". ")
            lastSpaceAt = nil
            return
        }
        proxy.insertText(" ")
        lastSpaceAt = now
    }

    private func toggleShift() -> Bool {
        let now = Date()
        let isDoubleTap = lastShiftAt.map { now.timeIntervalSince($0) < Self.doubleTapWindow } ?? false
        lastShiftAt = now
        switch shift {
        case .off: shift = .on
        case .on: shift = isDoubleTap ? .locked : .off
        case .locked: shift = .off
        }
        return true
    }

    // MARK: - Context

    /// Re-derive one-shot shift from where the cursor is. Called after every key and
    /// whenever the host moves the cursor, because the rule depends on what is
    /// before it, not on what was typed.
    ///
    /// Honours the field's `autocapitalizationType`: a username field asks for none,
    /// and giving it a capital anyway is the kind of thing people switch keyboards
    /// over. Caps lock is the user's explicit choice and is never overridden here.
    @discardableResult
    func refreshShift() -> Bool {
        guard shift != .locked, plane == .letters else { return false }
        let wanted: ShiftState = wantsCapital ? .on : .off
        guard wanted != shift else { return false }
        shift = wanted
        return true
    }

    private var wantsCapital: Bool {
        let before = proxy.documentContextBeforeInput ?? ""
        switch proxy.autocapitalizationType ?? .sentences {
        case .none:
            return false
        case .allCharacters:
            return true
        case .words:
            return before.isEmpty || before.last?.isWhitespace == true
        case .sentences:
            fallthrough
        @unknown default:
            if before.isEmpty { return true }
            let trimmed = before.trimmingCharacters(in: .whitespaces)
            if before.last?.isNewline == true || trimmed.isEmpty { return true }
            // "Sentence. " — punctuation then at least one space.
            guard before.last == " ", let end = trimmed.last else { return false }
            return ".!?".contains(end)
        }
    }

    // MARK: - Dictation

    /// Insert a finished dictation where the cursor is.
    ///
    /// With a space in front when the cursor sits right after a word — two dictations
    /// in a row, or a dictation after typing — so results never glue onto what is
    /// already there. The text itself is untouched: what the pipeline produced is
    /// what lands, and the Mac and iOS shells stay byte-identical on it.
    func insertDictation(_ text: String) {
        var out = text
        if let before = proxy.documentContextBeforeInput, let last = before.last,
           !last.isWhitespace, !last.isNewline,
           let first = out.first, !first.isPunctuation {
            out = " " + out
        }
        proxy.insertText(out)
        lastSpaceAt = nil
        refreshShift()
    }

    // MARK: - The return key

    /// What the host field wants its return key to say. The stock keyboard labels
    /// and tints it from the same property, so a search field gets a blue "search".
    var returnKeyTitle: String? {
        switch proxy.returnKeyType ?? .default {
        case .go: return "go"
        case .google, .search, .yahoo: return "search"
        case .join: return "join"
        case .next: return "next"
        case .route: return "route"
        case .send: return "send"
        case .done: return "done"
        case .emergencyCall: return "emergency"
        case .continue: return "continue"
        case .default: return nil
        @unknown default: return nil
        }
    }
}
