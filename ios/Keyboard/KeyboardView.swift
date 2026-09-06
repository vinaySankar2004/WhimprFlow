import UIKit

protocol KeyboardViewDelegate: AnyObject {
    /// A key was committed: on touch-up for most keys, on touch-down for shift and
    /// delete (delete then repeats while held).
    func keyboardView(_ view: KeyboardView, didCommit key: Key)
    /// A finger landed on a key. Feedback — click, haptic — belongs to the owner.
    func keyboardViewDidTouchKey(_ view: KeyboardView)
    func keyboardViewDidLongPressGlobe(_ view: KeyboardView)
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

    // Measured off Wispr Flow's keyboard on a 6.1" phone: shorter keys and wider
    // gaps than a first guess, which is most of why theirs reads as light and a
    // 43-pt key with an 11-pt gap read as heavy.
    static let keyHeight: CGFloat = 38
    static let rowGap: CGFloat = 14
    static let topPad: CGFloat = 8
    static let bottomPad: CGFloat = 4
    static let height: CGFloat = 4 * keyHeight + 3 * rowGap + topPad + bottomPad
    private let sideMargin: CGFloat = 3
    private let gap: CGFloat = 6

    private var rows: [[Key]] = []
    private var keyViews: [[KeyView]] = []
    private var active: [UITouch: KeyView] = [:]
    private var deleteRepeat: Timer?
    private var globeLongPress: Timer?
    private let popup = KeyPopup()

    override init(frame: CGRect) {
        super.init(frame: frame)
        isMultipleTouchEnabled = true
        clipsToBounds = false
        popup.isHidden = true
        rebuild()
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    // MARK: - Building

    private func rebuild() {
        keyViews.flatMap { $0 }.forEach { $0.removeFromSuperview() }
        popup.removeFromSuperview()
        rows = KeyboardLayout.rows(for: plane, includeGlobe: includeGlobe)
        keyViews = rows.map { row in
            row.map { key in
                let view = KeyView(key: key)
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
            view.style(shift: shift, plane: plane, returnTitle: returnTitle)
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
        let width = bounds.width
        let available = width - 2 * sideMargin
        let unit = (available - 9 * gap) / 10
        let side = unit * 1.4          // shift, delete, #+=, and the globe
        let wide = unit * 2.6          // the bottom row's plane key and return
        var y = Self.topPad

        for (rowIndex, row) in rows.enumerated() {
            let views = keyViews[rowIndex]
            var widths: [CGFloat] = []
            var gaps: [CGFloat] = Array(repeating: gap, count: max(0, row.count - 1))

            switch rowIndex {
            case 0, 1:
                widths = Array(repeating: unit, count: row.count)
            case 2:
                // Modifier, the middle keys, modifier — with a double gap either side
                // of the middle, as stock. Letters keep their width; punctuation on
                // the other planes spreads to fill.
                let middle = row.count - 2
                let inner = middle == 7
                    ? unit
                    : (available - 2 * side - 2 * (2 * gap) - CGFloat(middle - 1) * gap) / CGFloat(middle)
                widths = [side] + Array(repeating: inner, count: middle) + [side]
                gaps[0] = 2 * gap
                gaps[gaps.count - 1] = 2 * gap
                if middle == 7 {
                    // Whatever is left after stock widths goes into the two double gaps.
                    let used = 2 * side + 7 * unit + 6 * gap
                    let extra = max(0, available - used) / 2
                    gaps[0] = extra
                    gaps[gaps.count - 1] = extra
                }
            default:
                widths = row.map { key in
                    switch key {
                    case .plane: return includeGlobe ? side : wide
                    case .globe: return side
                    case .return: return wide
                    default: return 0 // space: filled below
                    }
                }
                let fixed = widths.reduce(0, +) + gaps.reduce(0, +)
                if let spaceIndex = row.firstIndex(of: .space) {
                    widths[spaceIndex] = available - fixed
                }
            }

            let rowWidth = widths.reduce(0, +) + gaps.reduce(0, +)
            var x = sideMargin + (available - rowWidth) / 2
            for (index, view) in views.enumerated() {
                view.frame = CGRect(x: x, y: y, width: widths[index], height: Self.keyHeight)
                x += widths[index] + (index < gaps.count ? gaps[index] : 0)
            }
            y += Self.keyHeight + Self.rowGap
        }
    }

    // MARK: - Touches

    private func keyView(at point: CGPoint) -> KeyView? {
        // Generous hit areas: the nearest key within the row, so a finger landing
        // in the gap still types. Rows are chosen by y, keys by x.
        for row in keyViews {
            guard let first = row.first else { continue }
            let top = first.frame.minY - Self.rowGap / 2
            let bottom = first.frame.maxY + Self.rowGap / 2
            guard point.y >= top, point.y < bottom else { continue }
            return row.min { abs($0.frame.midX - point.x) < abs($1.frame.midX - point.x) }
        }
        return nil
    }

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            guard let view = keyView(at: touch.location(in: self)) else { continue }
            active[touch] = view
            view.isPressed = true
            delegate?.keyboardViewDidTouchKey(self)
            switch view.key {
            case .shift:
                delegate?.keyboardView(self, didCommit: .shift)
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
                showPopup(over: view)
            default:
                break
            }
        }
    }

    override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            guard let current = active[touch] else { continue }
            // Shift and delete act on touch-down and do not slide; a finger that
            // wanders off delete should keep deleting, not start typing.
            if current.key == .shift || current.key == .delete || current.key == .globe { continue }
            guard let next = keyView(at: touch.location(in: self)), next !== current else { continue }
            guard !next.key.isModifier else { continue }
            current.isPressed = false
            next.isPressed = true
            active[touch] = next
            if case .character = next.key {
                showPopup(over: next)
            } else {
                popup.isHidden = true
            }
        }
    }

    override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
        for touch in touches {
            guard let view = active.removeValue(forKey: touch) else { continue }
            view.isPressed = false
            switch view.key {
            case .shift:
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
            guard let view = active.removeValue(forKey: touch) else { continue }
            view.isPressed = false
            if view.key == .delete { stopDeleteRepeat() }
        }
        globeLongPress?.invalidate()
        globeLongPress = nil
        if active.isEmpty { popup.isHidden = true }
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
        let height = Self.keyHeight + 14
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

    var isPressed = false {
        didSet { if isPressed != oldValue { paintBackground() } }
    }

    private var baseColor: UIColor = Palette.control
    private var isAction = false

    init(key: Key) {
        self.key = key
        super.init(frame: .zero)
        isUserInteractionEnabled = false
        layer.cornerRadius = 5.5
        layer.cornerCurve = .continuous
        // The stock keyboard's key shadow, which is most of why these read as keys.
        layer.shadowOpacity = 0.30
        layer.shadowOffset = CGSize(width: 0, height: 1)
        layer.shadowRadius = 0

        label.textAlignment = .center
        label.adjustsFontSizeToFitWidth = true
        label.minimumScaleFactor = 0.7
        image.contentMode = .center
        for view in [label, image] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            label.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -2),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            image.centerXAnchor.constraint(equalTo: centerXAnchor),
            image.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
        isAccessibilityElement = true
        accessibilityTraits = .keyboardKey
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    func style(shift: ShiftState, plane: Plane, returnTitle: String?) {
        layer.shadowColor = UIColor.black.cgColor
        label.textColor = Palette.textPrimary
        image.tintColor = Palette.textPrimary
        label.font = .systemFont(ofSize: 22, weight: .regular)
        label.text = nil
        image.image = nil
        isAction = false
        baseColor = key.isModifier ? Palette.modifierKey : Palette.control

        switch key {
        case let .character(text):
            let shown = (plane == .letters && shift.isActive) ? text.uppercased() : text
            label.text = shown
            accessibilityLabel = shown
        case .space:
            // Named, like Wispr Flow's space bar — the one place the keyboard says
            // whose it is.
            label.text = "WhimprFlow"
            label.font = .systemFont(ofSize: 14, weight: .regular)
            label.textColor = Palette.textSecondary
            accessibilityLabel = "space"
        case .shift:
            let symbol: String
            switch shift {
            case .off: symbol = "shift"
            case .on: symbol = "shift.fill"
            case .locked: symbol = "capslock.fill"
            }
            image.image = UIImage(systemName: symbol, withConfiguration: UIImage.SymbolConfiguration(pointSize: 17, weight: .regular))
            if shift.isActive { baseColor = Palette.control }
            accessibilityLabel = shift == .locked ? "caps lock on" : "shift"
        case .delete:
            image.image = UIImage(systemName: "delete.left", withConfiguration: UIImage.SymbolConfiguration(pointSize: 17, weight: .regular))
            accessibilityLabel = "delete"
        case .return:
            if let returnTitle {
                label.text = returnTitle
                label.font = .systemFont(ofSize: 15, weight: .regular)
                label.textColor = .white
                isAction = true
                baseColor = Palette.actionKey
                accessibilityLabel = returnTitle
            } else {
                image.image = UIImage(systemName: "return", withConfiguration: UIImage.SymbolConfiguration(pointSize: 16, weight: .regular))
                accessibilityLabel = "return"
            }
        case .globe:
            image.image = UIImage(systemName: "globe", withConfiguration: UIImage.SymbolConfiguration(pointSize: 17, weight: .regular))
            accessibilityLabel = "next keyboard"
        case .plane:
            label.text = key.title
            label.font = .systemFont(ofSize: 15, weight: .regular)
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
