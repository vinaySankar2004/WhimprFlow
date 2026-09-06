import UIKit

protocol KeyboardViewDelegate: AnyObject {
    /// A key was committed: on touch-up for most keys, on touch-down for shift and
    /// delete (delete then repeats while held).
    func keyboardView(_ view: KeyboardView, didCommit key: Key)
    /// A finger landed on a key. Feedback — click, haptic — belongs to the owner.
    func keyboardViewDidTouchKey(_ view: KeyboardView)
    func keyboardViewDidLongPressGlobe(_ view: KeyboardView)
    /// A finger drew a path across the letters instead of tapping one.
    func keyboardView(_ view: KeyboardView, didSwipe path: [CGPoint])
    /// Space-bar trackpad: move the cursor by this many characters.
    func keyboardView(_ view: KeyboardView, moveCursorBy offset: Int)
}

/// The key grid, drawn and hit-tested by hand.
///
/// Not a stack of `UIButton`s: the things that make a keyboard feel right — sliding
/// a finger onto the right key before lifting, two thumbs down at once, a popup over
/// the letter, delete repeating while held — are all touch handling across keys, and
/// a button knows only about itself. So one view owns every touch and maps it to the
/// key beneath it on each event.
///
/// Geometry follows the stock iPhone keyboard's proportions, which are also Wispr
/// Flow's: ten letter columns, the second row inset by half a key, wider modifiers,
/// the bottom row's plane key and return mirroring each other around the space bar.
final class KeyboardView: UIView, UIInputViewAudioFeedback {
    weak var delegate: KeyboardViewDelegate?

    /// Required for `UIDevice.playInputClick()` to make a sound from an extension.
    var enableInputClicksWhenVisible: Bool { true }

    var plane: Plane = .letters {
        didSet { if plane != oldValue { rebuild() } }
    }

    var shift: ShiftState = .off {
        didSet { if shift != oldValue { restyleAll() } }
    }

    /// Whether to draw a globe key. False on Face ID phones, where iOS draws its own
    /// in the strip below every third-party keyboard — drawing one here too gives
    /// the user two globes a centimetre apart.
    var includeGlobe = false {
        didSet { if includeGlobe != oldValue { rebuild() } }
    }

    /// The host field's return-key word ("search", "send"), or nil for a plain return.
    var returnTitle: String? {
        didSet { if returnTitle != oldValue { restyleAll() } }
    }

    // MARK: Geometry

    /// Everything about size, chosen per device and width rather than hard-coded:
    /// the phone numbers stretched across an iPad gave hairline gaps between keys
    /// three times as wide as they were tall, and no number row. These follow the
    /// stock keyboard on each.
    struct Metrics: Equatable {
        var keyHeight: CGFloat
        var rowGap: CGFloat
        var gap: CGFloat
        var sideMargin: CGFloat
        var topPad: CGFloat
        var bottomPad: CGFloat
        var cornerRadius: CGFloat
        var fontSize: CGFloat
        /// Letter popups: the phone has them, the iPad's keys are big enough not to.
        var popups: Bool
        /// The iPad arrangement (`KeyboardLayout.padRows`), with flick secondaries.
        var pad: Bool

        var height: CGFloat {
            4 * keyHeight + 3 * rowGap + topPad + bottomPad
        }

        /// Tuned on the phone in two rounds: 43/11 read as heavy, 38/14 as too much
        /// air. The stock keyboard's own 42/10 is where it settled.
        static let phone = Metrics(
            keyHeight: 42, rowGap: 10, gap: 6, sideMargin: 3, topPad: 8, bottomPad: 4,
            cornerRadius: 5.5, fontSize: 22, popups: true, pad: false
        )
        /// Measured off the stock keyboard on an 11" iPad: landscape keys are about
        /// 81 × 71 pt with 13-pt gaps; portrait about 55 × 55 with 10. Ours are a
        /// little shorter, since there is a bar above them the stock one lacks.
        static let padPortrait = Metrics(
            keyHeight: 58, rowGap: 10, gap: 10, sideMargin: 10, topPad: 10, bottomPad: 8,
            cornerRadius: 8, fontSize: 24, popups: false, pad: true
        )
        static let padLandscape = Metrics(
            keyHeight: 68, rowGap: 13, gap: 13, sideMargin: 14, topPad: 12, bottomPad: 10,
            cornerRadius: 10, fontSize: 26, popups: false, pad: true
        )

        static func `for`(width: CGFloat) -> Metrics {
            guard UIDevice.current.userInterfaceIdiom == .pad else { return .phone }
            return width >= 1000 ? .padLandscape : .padPortrait
        }
    }

    var metrics: Metrics = .phone {
        didSet { if metrics != oldValue { rebuild() } }
    }

    private var rows: [[Key]] = []
    private var keyViews: [[KeyView]] = []
    private var active: [UITouch: KeyView] = [:]
    private var deleteRepeat: Timer?
    private var globeLongPress: Timer?
    private let popup = KeyPopup()

    /// Swipe typing. A touch that starts on a letter records its path; once it has
    /// travelled further than a slide-to-the-next-key could, it stops being a tap
    /// and becomes a swipe, drawn as a trail and decoded on release.
    private var swipePaths: [UITouch: [CGPoint]] = [:]
    private var swiping: Set<UITouch> = []
    /// iPad flick: a short downward drag on a letter types its secondary label.
    private var touchStarts: [UITouch: CGPoint] = [:]
    private var flicked: Set<UITouch> = []
    /// Space-bar trackpad: hold the space bar, then drag to move the cursor, as the
    /// stock keyboard does. The keys dim to say the mode changed; lifting ends it
    /// without typing a space.
    private var spaceHold: Timer?
    private var cursorTouch: UITouch?
    private var cursorLastX: CGFloat = 0
    private let cursorStep: CGFloat = 9
    private let trail = CAShapeLayer()
    private let hint = UILabel()
    private var hintTimer: Timer?

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        clipsToBounds = false
        popup.isHidden = true

        trail.fillColor = nil
        trail.strokeColor = Palette.accent.cgColor
        trail.lineWidth = 7
        trail.lineCap = .round
        trail.lineJoin = .round
        trail.opacity = 0.55
        layer.addSublayer(trail)

        hint.font = .systemFont(ofSize: 14, weight: .medium)
        hint.textColor = Palette.textPrimary
        hint.backgroundColor = Palette.control
        hint.textAlignment = .center
        hint.layer.cornerRadius = 14
        hint.layer.cornerCurve = .continuous
        hint.layer.masksToBounds = true
        hint.alpha = 0
        addSubview(hint)
        rebuild()
    }

    /// The letter keys' centres, a=0…z=25, for the swipe decoder. Empty off the
    /// letters plane, where swiping is not offered.
    func letterCentres() -> [Int: CGPoint] {
        guard plane == .letters else { return [:] }
        var centres: [Int: CGPoint] = [:]
        for view in keyViews.flatMap({ $0 }) {
            if case let .character(text) = view.key, text.count == 1,
               let scalar = text.unicodeScalars.first, scalar.value >= 97, scalar.value <= 122 {
                centres[Int(scalar.value) - 97] = view.center
            }
        }
        return centres
    }

    var letterKeyWidth: CGFloat {
        // The q row, whichever row it is: the number row above it is the same width.
        keyViews.first?.first?.frame.width ?? 33
    }

    /// A short line above the space bar — "teh → the" — gone after a moment.
    func showHint(_ text: String) {
        hintTimer?.invalidate()
        hint.text = "  \(text)  "
        hint.sizeToFit()
        let width = hint.frame.width + 12
        let space = keyViews.last?.first(where: { $0.key == .space })
        let anchorX = space?.frame.midX ?? bounds.midX
        let anchorY = (space?.frame.minY ?? bounds.maxY) - 8
        hint.frame = CGRect(x: anchorX - width / 2, y: anchorY - 28, width: width, height: 28)
        bringSubviewToFront(hint)
        UIView.animate(withDuration: 0.12) { self.hint.alpha = 1 }
        hintTimer = Timer.scheduledTimer(withTimeInterval: 1.4, repeats: false) { [weak self] _ in
            UIView.animate(withDuration: 0.25) { self?.hint.alpha = 0 }
        }
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    // MARK: - Building

    private func rebuild() {
        keyViews.flatMap { $0 }.forEach { $0.removeFromSuperview() }
        popup.removeFromSuperview()
        rows = metrics.pad
            ? KeyboardLayout.padRows(for: plane, includeGlobe: includeGlobe)
            : KeyboardLayout.rows(for: plane, includeGlobe: includeGlobe)
        keyViews = rows.map { row in
            row.map { key in
                let view = KeyView(key: key, cornerRadius: metrics.cornerRadius)
                if metrics.pad, plane == .letters, case let .character(text) = key {
                    view.secondary = KeyboardLayout.padSecondary[text]
                }
                addSubview(view)
                return view
            }
        }
        addSubview(popup)
        active.removeAll()
        restyleAll()
        setNeedsLayout()
    }

    private func restyleAll() {
        for view in keyViews.flatMap({ $0 }) {
            view.style(shift: shift, plane: plane, returnTitle: returnTitle, fontSize: metrics.fontSize)
        }
    }

    /// Repaint after a light/dark flip. Dynamic colours re-resolve themselves;
    /// the shadow's `CGColor` does not.
    func repaint() {
        restyleAll()
    }

    // MARK: - Layout

    override func layoutSubviews() {
        super.layoutSubviews()
        if metrics.pad {
            layoutPad()
            return
        }
        let m = metrics
        let width = bounds.width
        let available = width - 2 * m.sideMargin
        let unit = (available - 9 * m.gap) / 10
        let side = unit * 1.4          // shift, delete, #+=, and the globe
        let pad = false
        var y = m.topPad

        for (rowIndex, row) in rows.enumerated() {
            let views = keyViews[rowIndex]
            var widths: [CGFloat] = []
            var gaps: [CGFloat] = Array(repeating: m.gap, count: max(0, row.count - 1))
            let isModifierRow = row.count > 2 && row[row.count - 1] == .delete
            let isBottomRow = row.contains(.space)

            if isBottomRow {
                // On the phone the plane key and return mirror each other around the
                // space bar; on the iPad's width that would make them the size of a
                // hand, so they stay near a key and a half, as stock does there.
                widths = row.map { key in
                    switch key {
                    case .plane: return pad ? unit * 1.4 : (includeGlobe ? side : unit * 2.6)
                    case .globe: return pad ? unit : side
                    case .return: return pad ? unit * 1.8 : unit * 2.6
                    default: return 0 // space: filled below
                    }
                }
                let fixed = widths.reduce(0, +) + gaps.reduce(0, +)
                if let spaceIndex = row.firstIndex(of: .space) {
                    widths[spaceIndex] = available - fixed
                }
            } else if isModifierRow {
                // Modifier, the middle keys, modifier — with a double gap either side
                // of the middle, as stock. Letters keep their width; punctuation on
                // the other planes spreads to fill.
                let middle = row.count - 2
                let inner = middle == 7
                    ? unit
                    : (available - 2 * side - 2 * (2 * m.gap) - CGFloat(middle - 1) * m.gap) / CGFloat(middle)
                widths = [side] + Array(repeating: inner, count: middle) + [side]
                gaps[0] = 2 * m.gap
                gaps[gaps.count - 1] = 2 * m.gap
                if middle == 7 {
                    // Whatever is left after stock widths goes into the two double gaps.
                    let used = 2 * side + 7 * unit + 6 * m.gap
                    let extra = max(0, available - used) / 2
                    gaps[0] = extra
                    gaps[gaps.count - 1] = extra
                }
            } else {
                widths = Array(repeating: unit, count: row.count)
            }

            let rowWidth = widths.reduce(0, +) + gaps.reduce(0, +)
            var x = m.sideMargin + (available - rowWidth) / 2
            for (index, view) in views.enumerated() {
                view.frame = CGRect(x: x, y: y, width: widths[index], height: m.keyHeight)
                x += widths[index] + (index < gaps.count ? gaps[index] : 0)
            }
            y += m.keyHeight + m.rowGap
        }
    }

    /// The iPad grid. Every row is side key · middle keys · side key, and the side
    /// keys' widths are the stock keyboard's, in letter-key units: a letter key is
    /// what is left of the top row after tab and delete, and the other rows come
    /// out the same width by the factors below.
    private func layoutPad() {
        let m = metrics
        let available = bounds.width - 2 * m.sideMargin
        // Top row: 10 letters, tab and delete at 1.3 each, 11 gaps.
        let unit = (available - 11 * m.gap) / 12.6
        var y = m.topPad

        for (rowIndex, row) in rows.enumerated() {
            let views = keyViews[rowIndex]
            var widths: [CGFloat] = row.enumerated().map { index, key in
                let isFirst = index == 0
                let isLast = index == row.count - 1
                switch key {
                case .character: return unit
                case .tab: return unit * 1.3
                case .delete: return row.count == 11 ? unit * 2.6 + m.gap : unit * 1.3
                case .shift, .capsLock, .plane where isFirst:
                    return rowIndex == 2 ? unit * 2.15 : unit * 1.65
                case .shift, .plane where isLast && rowIndex == 2:
                    return unit * 1.6
                case .return: return unit * 2.1
                case .globe, .dictate: return unit * 1.05
                case .plane: return isLast || rowIndex == 3 && index >= row.count - 2 ? unit * 1.5 : unit * 1.05
                case .hide: return unit * 1.5
                case .space: return 0
                }
            }
            let gaps = CGFloat(max(0, row.count - 1)) * m.gap
            if let spaceIndex = row.firstIndex(of: .space) {
                widths[spaceIndex] = available - widths.reduce(0, +) - gaps
            }
            let rowWidth = widths.reduce(0, +) + gaps
            var x = m.sideMargin + (available - rowWidth) / 2
            for (index, view) in views.enumerated() {
                view.frame = CGRect(x: x, y: y, width: widths[index], height: m.keyHeight)
                x += widths[index] + m.gap
            }
            y += m.keyHeight + m.rowGap
        }
    }

    // MARK: - Touches

    private func keyView(at point: CGPoint) -> KeyView? {
        // Generous hit areas: the nearest key within the row, so a finger landing
        // in the gap still types. Rows are chosen by y, keys by x.
        for row in keyViews {
            guard let first = row.first else { continue }
            let top = first.frame.minY - metrics.rowGap / 2
            let bottom = first.frame.maxY + metrics.rowGap / 2
            guard point.y >= top, point.y < bottom else { continue }
            return row.min { abs($0.frame.midX - point.x) < abs($1.frame.midX - point.x) }
        }
        return nil
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            guard let view = keyView(at: touch.location(in: self)) else { continue }
            active[touch] = view
            touchStarts[touch] = touch.location(in: self)
            view.isPressed = true
            delegate?.keyboardViewDidTouchKey(self)
            switch view.key {
            case .shift, .capsLock:
                delegate?.keyboardView(self, didCommit: view.key)
            case .delete:
                delegate?.keyboardView(self, didCommit: .delete)
                startDeleteRepeat()
            case .globe:
                globeLongPress?.invalidate()
                globeLongPress = Timer.scheduledTimer(withTimeInterval: 0.45, repeats: false) { [weak self] _ in
                    guard let self else { return }
                    self.active = self.active.filter { $0.value.key != .globe }
                    view.isPressed = false
                    self.delegate?.keyboardViewDidLongPressGlobe(self)
                }
            case .character:
                if metrics.popups { showPopup(over: view) }
                if plane == .letters { swipePaths[touch] = [touch.location(in: self)] }
            case .space:
                spaceHold?.invalidate()
                spaceHold = Timer.scheduledTimer(withTimeInterval: 0.35, repeats: false) { [weak self] _ in
                    guard let self, self.active[touch] === view else { return }
                    self.cursorTouch = touch
                    self.cursorLastX = touch.location(in: self).x
                    self.setCursorMode(true)
                }
            default:
                break
            }
        }
    }

    private func setCursorMode(_ on: Bool) {
        for view in keyViews.flatMap({ $0 }) where view.key != .space {
            UIView.animate(withDuration: 0.12) { view.alpha = on ? 0.3 : 1 }
        }
        if on { UISelectionFeedbackGenerator().selectionChanged() }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            if touch === cursorTouch {
                let x = touch.location(in: self).x
                let steps = Int((x - cursorLastX) / cursorStep)
                if steps != 0 {
                    cursorLastX += CGFloat(steps) * cursorStep
                    delegate?.keyboardView(self, moveCursorBy: steps)
                }
                continue
            }
            // A finger that leaves the space bar before the hold lands is sliding,
            // not asking for the trackpad.
            if spaceHold != nil, active[touch]?.key == .space, let start = touchStarts[touch],
               hypot(touch.location(in: self).x - start.x, touch.location(in: self).y - start.y) > 12 {
                spaceHold?.invalidate()
                spaceHold = nil
            }
            // A flick: down a little, not sideways, on a key with a secondary. It
            // wins over swipe typing because it is decided within the first key.
            if metrics.pad, !flicked.contains(touch), !swiping.contains(touch),
               let start = touchStarts[touch], let view = active[touch], view.secondary != nil {
                let point = touch.location(in: self)
                if point.y - start.y > 22, abs(point.x - start.x) < 28 {
                    flicked.insert(touch)
                    swipePaths.removeValue(forKey: touch)
                    view.showsSecondary = true
                    continue
                }
            }
            if flicked.contains(touch) { continue }
            if var path = swipePaths[touch] {
                let point = touch.location(in: self)
                path.append(point)
                swipePaths[touch] = path
                if swiping.contains(touch) {
                    drawTrail(path)
                    continue
                }
                // Further than a finger slides to correct a tap: this is a swipe.
                var length: CGFloat = 0
                for index in 1..<path.count { length += hypot(path[index].x - path[index - 1].x, path[index].y - path[index - 1].y) }
                if length > letterKeyWidth * 1.6 {
                    swiping.insert(touch)
                    if let view = active.removeValue(forKey: touch) { view.isPressed = false }
                    if active.isEmpty { popup.isHidden = true }
                    drawTrail(path)
                    continue
                }
            }
            guard let current = active[touch] else { continue }
            // Shift and delete act on touch-down and do not slide; a finger that
            // wanders off delete should keep deleting, not start typing.
            if current.key == .shift || current.key == .capsLock || current.key == .delete || current.key == .globe { continue }
            guard let next = keyView(at: touch.location(in: self)), next !== current else { continue }
            guard !next.key.isModifier else { continue }
            current.isPressed = false
            next.isPressed = true
            active[touch] = next
            if case .character = next.key, metrics.popups {
                showPopup(over: next)
            } else {
                popup.isHidden = true
            }
        }
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            touchStarts.removeValue(forKey: touch)
            if touch === cursorTouch {
                cursorTouch = nil
                setCursorMode(false)
                active.removeValue(forKey: touch)?.isPressed = false
                continue
            }
            if active[touch]?.key == .space {
                spaceHold?.invalidate()
                spaceHold = nil
            }
            if flicked.remove(touch) != nil, let view = active.removeValue(forKey: touch) {
                view.isPressed = false
                view.showsSecondary = false
                if let secondary = view.secondary {
                    delegate?.keyboardView(self, didCommit: .character(secondary))
                }
                continue
            }
            if swiping.remove(touch) != nil, let path = swipePaths.removeValue(forKey: touch) {
                clearTrail()
                delegate?.keyboardView(self, didSwipe: path)
                continue
            }
            swipePaths.removeValue(forKey: touch)
            guard let view = active.removeValue(forKey: touch) else { continue }
            view.isPressed = false
            switch view.key {
            case .shift, .capsLock:
                break
            case .delete:
                stopDeleteRepeat()
            case .globe:
                globeLongPress?.invalidate()
                globeLongPress = nil
                delegate?.keyboardView(self, didCommit: .globe)
            default:
                delegate?.keyboardView(self, didCommit: view.key)
            }
        }
        if active.isEmpty { popup.isHidden = true }
    }

    override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            touchStarts.removeValue(forKey: touch)
            if touch === cursorTouch {
                cursorTouch = nil
                setCursorMode(false)
            }
            spaceHold?.invalidate()
            spaceHold = nil
            if flicked.remove(touch) != nil { active[touch]?.showsSecondary = false }
            swiping.remove(touch)
            swipePaths.removeValue(forKey: touch)
            clearTrail()
            guard let view = active.removeValue(forKey: touch) else { continue }
            view.isPressed = false
            if view.key == .delete { stopDeleteRepeat() }
        }
        globeLongPress?.invalidate()
        globeLongPress = nil
        if active.isEmpty { popup.isHidden = true }
    }

    private func drawTrail(_ path: [CGPoint]) {
        // Only the last stretch, so the trail reads as motion rather than a scrawl.
        let recent = path.suffix(28)
        let bezier = UIBezierPath()
        for (index, point) in recent.enumerated() {
            if index == 0 { bezier.move(to: point) } else { bezier.addLine(to: point) }
        }
        trail.strokeColor = Palette.accent.resolvedColor(with: traitCollection).cgColor
        trail.path = bezier.cgPath
    }

    private func clearTrail() {
        trail.path = nil
    }

    private func startDeleteRepeat() {
        stopDeleteRepeat()
        deleteRepeat = Timer.scheduledTimer(withTimeInterval: 0.4, repeats: false) { [weak self] _ in
            guard let self else { return }
            self.deleteRepeat = Timer.scheduledTimer(withTimeInterval: 0.08, repeats: true) { [weak self] _ in
                guard let self else { return }
                self.delegate?.keyboardView(self, didCommit: .delete)
            }
        }
    }

    private func stopDeleteRepeat() {
        deleteRepeat?.invalidate()
        deleteRepeat = nil
    }

    private func showPopup(over view: KeyView) {
        guard case let .character(text) = view.key else { return }
        popup.text = (shift.isActive && plane == .letters) ? text.uppercased() : text
        let width = view.frame.width + 20
        let height = metrics.keyHeight + 14
        popup.frame = CGRect(
            x: min(max(view.frame.midX - width / 2, 2), bounds.width - width - 2),
            y: view.frame.minY - height - 4,
            width: width,
            height: height
        )
        popup.isHidden = false
        bringSubviewToFront(popup)
    }
}

// MARK: - One key

final class KeyView: UIView {
    let key: Key
    private let label = UILabel()
    private let image = UIImageView()
    private let secondaryLabel = UILabel()

    /// The flick character printed small at the top (iPad letters).
    var secondary: String? {
        didSet {
            secondaryLabel.text = secondary
            secondaryLabel.isHidden = secondary == nil
        }
    }

    /// While a flick is in progress the secondary takes the key, as stock animates.
    var showsSecondary = false {
        didSet {
            guard let secondary else { return }
            label.text = showsSecondary ? secondary : primaryText
            secondaryLabel.isHidden = showsSecondary
        }
    }
    private var primaryText: String?

    var isPressed = false {
        didSet { if isPressed != oldValue { paintBackground() } }
    }

    private var baseColor: UIColor = Palette.control
    private var isAction = false

    init(key: Key, cornerRadius: CGFloat) {
        self.key = key
        super.init(frame: .zero)
        isUserInteractionEnabled = false
        layer.cornerRadius = cornerRadius
        layer.cornerCurve = .continuous
        // The stock keyboard's key shadow, which is most of why these read as keys.
        layer.shadowOpacity = 0.30
        layer.shadowOffset = CGSize(width: 0, height: 1)
        layer.shadowRadius = 0

        label.textAlignment = .center
        label.adjustsFontSizeToFitWidth = true
        label.minimumScaleFactor = 0.7
        image.contentMode = .center
        secondaryLabel.textAlignment = .center
        secondaryLabel.font = .systemFont(ofSize: 13, weight: .regular)
        secondaryLabel.textColor = Palette.textSecondary
        secondaryLabel.isHidden = true
        for view in [label, image, secondaryLabel] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            label.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -2),
            label.centerYAnchor.constraint(equalTo: centerYAnchor, constant: 0),
            image.centerXAnchor.constraint(equalTo: centerXAnchor),
            image.centerYAnchor.constraint(equalTo: centerYAnchor),
            secondaryLabel.topAnchor.constraint(equalTo: topAnchor, constant: 5),
            secondaryLabel.centerXAnchor.constraint(equalTo: centerXAnchor),
        ])
        isAccessibilityElement = true
        accessibilityTraits = .keyboardKey
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    func style(shift: ShiftState, plane: Plane, returnTitle: String?, fontSize: CGFloat) {
        layer.shadowColor = UIColor.black.cgColor
        label.textColor = Palette.textPrimary
        image.tintColor = Palette.textPrimary
        label.font = .systemFont(ofSize: fontSize, weight: .regular)
        // Glyphs and words scale with the letters, a little under them.
        let symbolSize = fontSize * 0.78
        let wordSize = fontSize * 0.68
        label.text = nil
        image.image = nil
        isAction = false
        baseColor = key.isModifier ? Palette.modifierKey : Palette.control

        switch key {
        case let .character(text):
            let shown = (plane == .letters && shift.isActive) ? text.uppercased() : text
            label.text = shown
            primaryText = shown
            // With a secondary printed above, the letter sits a little low, as stock.
            if secondary != nil { label.transform = CGAffineTransform(translationX: 0, y: 5) }
            accessibilityLabel = shown
        case .space:
            // Named, like Wispr Flow's space bar — the one place the keyboard says
            // whose it is.
            label.text = "WhimprFlow"
            label.font = .systemFont(ofSize: wordSize, weight: .regular)
            label.textColor = Palette.textSecondary
            accessibilityLabel = "space"
        case .shift:
            let symbol: String
            switch shift {
            case .off: symbol = "shift"
            case .on: symbol = "shift.fill"
            case .locked: symbol = "capslock.fill"
            }
            image.image = UIImage(systemName: symbol, withConfiguration: UIImage.SymbolConfiguration(pointSize: symbolSize, weight: .regular))
            if shift.isActive { baseColor = Palette.control }
            accessibilityLabel = shift == .locked ? "caps lock on" : "shift"
        case .delete:
            image.image = UIImage(systemName: "delete.left", withConfiguration: UIImage.SymbolConfiguration(pointSize: symbolSize, weight: .regular))
            accessibilityLabel = "delete"
        case .return:
            if let returnTitle {
                label.text = returnTitle
                label.font = .systemFont(ofSize: wordSize + 1, weight: .regular)
                label.textColor = .white
                isAction = true
                baseColor = Palette.actionKey
                accessibilityLabel = returnTitle
            } else {
                image.image = UIImage(systemName: "return", withConfiguration: UIImage.SymbolConfiguration(pointSize: symbolSize - 1, weight: .regular))
                accessibilityLabel = "return"
            }
        case .globe:
            image.image = UIImage(systemName: "globe", withConfiguration: UIImage.SymbolConfiguration(pointSize: symbolSize, weight: .regular))
            accessibilityLabel = "next keyboard"
        case .tab:
            image.image = UIImage(systemName: "arrow.right.to.line", withConfiguration: UIImage.SymbolConfiguration(pointSize: symbolSize - 2, weight: .regular))
            accessibilityLabel = "tab"
        case .capsLock:
            let locked = shift == .locked
            image.image = UIImage(systemName: locked ? "capslock.fill" : "capslock", withConfiguration: UIImage.SymbolConfiguration(pointSize: symbolSize, weight: .regular))
            if locked { baseColor = Palette.control }
            accessibilityLabel = locked ? "caps lock on" : "caps lock"
        case .hide:
            image.image = UIImage(systemName: "keyboard.chevron.compact.down", withConfiguration: UIImage.SymbolConfiguration(pointSize: symbolSize, weight: .regular))
            accessibilityLabel = "hide keyboard"
        case .dictate:
            image.image = UIImage(systemName: "mic", withConfiguration: UIImage.SymbolConfiguration(pointSize: symbolSize, weight: .regular))
            accessibilityLabel = "dictate"
        case .plane:
            label.text = key.title
            label.font = .systemFont(ofSize: wordSize + 1, weight: .regular)
            accessibilityLabel = key.title
        }
        paintBackground()
    }

    private func paintBackground() {
        if isPressed {
            backgroundColor = isAction ? Palette.actionKey.withAlphaComponent(0.75) : Palette.keyPressed
        } else {
            backgroundColor = baseColor
        }
    }
}

/// The enlarged character shown above a key while it is held.
final class KeyPopup: UIView {
    private let label = UILabel()

    var text: String? {
        didSet { label.text = text }
    }

    override init(frame: CGRect) {
        super.init(frame: frame)
        isUserInteractionEnabled = false
        backgroundColor = Palette.control
        layer.cornerRadius = 8
        layer.cornerCurve = .continuous
        layer.shadowColor = UIColor.black.cgColor
        layer.shadowOpacity = 0.35
        layer.shadowOffset = CGSize(width: 0, height: 2)
        layer.shadowRadius = 4
        label.font = .systemFont(ofSize: 32, weight: .regular)
        label.textColor = Palette.textPrimary
        label.textAlignment = .center
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        NSLayoutConstraint.activate([
            label.centerXAnchor.constraint(equalTo: centerXAnchor),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError("not used") }
}
