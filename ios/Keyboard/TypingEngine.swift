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
/// # Autocorrect
///
/// Apple's `UITextChecker` is available to a keyboard extension, so a misspelled word
/// can be corrected on the space or punctuation that ends it. Deliberately
/// conservative: only a word typed entirely on this keyboard, only when the checker
/// flags the whole word, only its first guess, only within a small edit distance and
/// keeping the first letter — and delete right after a correction puts the typed
/// word back and teaches the checker it. Anything looser rewrites names and slang,
/// which is the reason people turn autocorrect off.
///
/// This type decides; `KeyboardView` draws and `KeyboardViewController` connects.
final class TypingEngine {
    struct Correction {
        let from: String
        let to: String
    }

    private let proxy: UITextDocumentProxy
    private let checker = UITextChecker()
    private let language = "en_US"

    private(set) var shift: ShiftState = .off
    private(set) var plane: Plane = .letters

    /// From Settings; the controller refreshes it on each appearance.
    var autocorrectEnabled = true
    /// Told about each correction, so the keyboard can show what changed.
    var onCorrection: ((Correction) -> Void)?

    /// Letters typed on this keyboard since the last separator. Autocorrect touches a
    /// word only when every letter of it was typed here: a dictated or pasted word,
    /// or one the cursor was moved into, is not ours to second-guess.
    private var typedWordLength = 0
    /// The last correction, while it can still be undone by delete.
    private var lastCorrection: (original: String, corrected: String, separator: String)?
    /// The last swiped word as inserted, so an alternative can replace it.
    private var lastSwipe: (inserted: String, leadingSpace: Bool)?

    /// Shift was set by a tap on the key, not by the sentence rule. The rule stands
    /// down until a letter consumes it: without this, every key commit re-derived
    /// shift from the text and undid the tap at once — and the second tap of a
    /// caps-lock double tap never found the first one still on.
    private var manualShift = false

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
        if key != .shift { lastSwipe = nil }
        switch key {
        case let .character(text):
            if let first = text.first, first.isLetter || first == "'" {
                lastCorrection = nil
                insertCharacter(text)
                typedWordLength += 1
            } else {
                // Punctuation ends a word, so it is where a typo gets fixed.
                if ".,?!;:".contains(text) { autocorrectCurrentWord(separator: text) } else { lastCorrection = nil }
                insertCharacter(text)
                typedWordLength = 0
            }
            return settleAfterCharacter(text)
        case .space:
            // Only the first space corrects; the second is the full-stop rule's.
            if lastSpaceAt == nil { autocorrectCurrentWord(separator: " ") }
            insertSpace()
            typedWordLength = 0
            return refreshShift()
        case .return:
            autocorrectCurrentWord(separator: "\n")
            proxy.insertText("\n")
            typedWordLength = 0
            return refreshShift()
        case .delete:
            if undoCorrectionIfPossible() { return refreshShift() }
            proxy.deleteBackward()
            typedWordLength = max(0, typedWordLength - 1)
            return refreshShift()
        case .shift:
            return toggleShift()
        case .capsLock:
            manualShift = true
            shift = shift == .locked ? .off : .locked
            return true
        case .tab:
            proxy.insertText("\t")
            typedWordLength = 0
            return false
        case .hide, .dictate:
            return false // the controller's, not text
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
        guard plane == .letters, text.first?.isLetter == true else { return false }
        // A letter consumes a tapped shift, on or off; the sentence rule resumes.
        manualShift = false
        if shift == .on {
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
        manualShift = true
        switch shift {
        case .off: shift = .on
        case .on: shift = isDoubleTap ? .locked : .off
        case .locked: shift = .off
        }
        return true
    }

    // MARK: - Autocorrect

    /// Fix the word before the cursor, if it is clearly a typo. See the type comment
    /// for every condition and why it is there.
    private func autocorrectCurrentWord(separator: String) {
        lastCorrection = nil
        guard autocorrectEnabled, (proxy.autocorrectionType ?? .default) != .no else { return }
        guard let before = proxy.documentContextBeforeInput else { return }
        let word = String(before.reversed().prefix { $0.isLetter || $0 == "'" }.reversed())
        guard word.count >= 2, typedWordLength >= word.count else { return }
        // ALL CAPS and iPhone-style internal capitals are choices, not typos.
        if word.count > 1, word == word.uppercased() { return }
        let tail = word.dropFirst()
        guard tail == tail.lowercased() else { return }

        let whole = NSRange(location: 0, length: (word as NSString).length)
        let flagged = checker.rangeOfMisspelledWord(in: word, range: whole, startingAt: 0, wrap: false, language: language)
        guard flagged.location != NSNotFound, flagged == whole else { return }
        guard let guess = checker.guesses(forWordRange: whole, in: word, language: language)?.first,
              !guess.contains(" "),
              guess.first?.lowercased() == word.first?.lowercased() else { return }
        let distance = Self.editDistance(word.lowercased(), guess.lowercased())
        guard distance > 0, distance <= (word.count <= 4 ? 1 : 2) else { return }

        var corrected = guess
        if word.first?.isUppercase == true, let first = corrected.first {
            corrected = String(first).uppercased() + corrected.dropFirst()
        }
        for _ in 0..<word.count { proxy.deleteBackward() }
        proxy.insertText(corrected)
        lastCorrection = (word, corrected, separator)
        typedWordLength = 0
        onCorrection?(Correction(from: word, to: corrected))
    }

    /// Delete straight after a correction restores what was typed — and teaches the
    /// checker the word, so the same fight is not had twice.
    private func undoCorrectionIfPossible() -> Bool {
        guard let last = lastCorrection else { return false }
        lastCorrection = nil
        let expected = last.corrected + last.separator
        guard let before = proxy.documentContextBeforeInput, before.hasSuffix(expected) else { return false }
        for _ in 0..<expected.count { proxy.deleteBackward() }
        proxy.insertText(last.original + last.separator)
        UITextChecker.learnWord(last.original)
        typedWordLength = 0
        return true
    }

    /// Damerau–Levenshtein with adjacent transposition, the distance typos have.
    static func editDistance(_ a: String, _ b: String) -> Int {
        let x = Array(a), y = Array(b)
        if x.isEmpty { return y.count }
        if y.isEmpty { return x.count }
        var d = Array(repeating: Array(repeating: 0, count: y.count + 1), count: x.count + 1)
        for i in 0...x.count { d[i][0] = i }
        for j in 0...y.count { d[0][j] = j }
        for i in 1...x.count {
            for j in 1...y.count {
                let cost = x[i - 1] == y[j - 1] ? 0 : 1
                d[i][j] = min(d[i - 1][j] + 1, d[i][j - 1] + 1, d[i - 1][j - 1] + cost)
                if i > 1, j > 1, x[i - 1] == y[j - 2], x[i - 2] == y[j - 1] {
                    d[i][j] = min(d[i][j], d[i - 2][j - 2] + 1)
                }
            }
        }
        return d[x.count][y.count]
    }

    // MARK: - Swipe

    /// Insert a word drawn with a swipe. Cased by shift like a typed word, with a
    /// space in front when the cursor sits after a word, so consecutive swipes come
    /// out as a sentence without touching the space bar.
    func insertSwipe(_ word: String) {
        let cased = caseForShift(word)
        let leading = needsLeadingSpace
        proxy.insertText((leading ? " " : "") + cased)
        lastSwipe = (cased, leading)
        lastCorrection = nil
        lastSpaceAt = nil
        typedWordLength = 0
        manualShift = false
        if shift == .on { shift = .off }
        refreshShift()
    }

    /// Swap the last swiped word for one of its alternatives.
    func replaceLastSwipe(with word: String) {
        guard let last = lastSwipe,
              let before = proxy.documentContextBeforeInput, before.hasSuffix(last.inserted) else { return }
        for _ in 0..<last.inserted.count { proxy.deleteBackward() }
        let cased = last.inserted.first?.isUppercase == true
            ? (last.inserted == last.inserted.uppercased() ? word.uppercased() : word.prefix(1).uppercased() + word.dropFirst())
            : word
        proxy.insertText(cased)
        lastSwipe = (cased, last.leadingSpace)
    }

    private func caseForShift(_ word: String) -> String {
        switch shift {
        case .off: return word
        case .on: return word.prefix(1).uppercased() + word.dropFirst()
        case .locked: return word.uppercased()
        }
    }

    private var needsLeadingSpace: Bool {
        guard let last = proxy.documentContextBeforeInput?.last else { return false }
        return !last.isWhitespace && !last.isNewline && last != "(" && last != "\"" && last != "'"
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
        guard shift != .locked, !manualShift, plane == .letters else { return false }
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
        lastCorrection = nil
        lastSwipe = nil
        typedWordLength = 0
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
