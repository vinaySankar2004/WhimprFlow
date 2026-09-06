import UIKit

/// The strip above the keys: menu · level pill · mic.
///
/// The same three things in the same three places as Wispr Flow's bar, which is the
/// arrangement people who have used a dictation keyboard already know. While
/// listening the ends become discard and finish and the pill dims: the bar is the
/// one part of the keyboard that is always there, so it is where control lives.
final class TopBar: UIView {
    enum Mode: Equatable {
        case typing, listening, transcribing
        /// A notice is covering the keys; the left control brings them back.
        case notice
    }

    var onOpenApp: (() -> Void)?
    var onRelease: (() -> Void)?
    var onPill: (() -> Void)?
    var onMic: (() -> Void)?
    var onCancel: (() -> Void)?
    var onConfirm: (() -> Void)?
    var onDismissNotice: (() -> Void)?

    /// Wispr's bar is about this tall with a 44-pt pill centred in it; the air
    /// above and below is what keeps the pill from crowding the top row.
    static let height: CGFloat = 60

    private let menuButton = UIButton(type: .system)
    private let cancelButton = UIButton(type: .custom)
    private let pill = UIButton(type: .custom)
    private let rightButton = UIButton(type: .custom)
    private let spinner = UIActivityIndicatorView(style: .medium)

    private(set) var mode: Mode = .typing
    private var level: CleanupLevel = .light
    private var canRelease = false

    override init(frame: CGRect) {
        super.init(frame: frame)
        build()
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    // MARK: - Build

    private func build() {
        // ≡ — the way into the app, and the home of the things that do not deserve a
        // key: the level as a list, and releasing the mic.
        menuButton.setImage(
            UIImage(systemName: "line.3.horizontal",
                    withConfiguration: UIImage.SymbolConfiguration(pointSize: 20, weight: .medium)),
            for: .normal
        )
        menuButton.tintColor = Palette.textPrimary
        menuButton.accessibilityLabel = "WhimprFlow menu"
        menuButton.showsMenuAsPrimaryAction = true
        menuButton.menu = UIMenu(children: [
            UIDeferredMenuElement.uncached { [weak self] completion in
                completion(self?.menuItems() ?? [])
            }
        ])

        // ✕ — discard, in the menu's place while listening.
        cancelButton.setImage(
            UIImage(systemName: "xmark",
                    withConfiguration: UIImage.SymbolConfiguration(pointSize: 17, weight: .semibold)),
            for: .normal
        )
        cancelButton.tintColor = Palette.textPrimary
        cancelButton.backgroundColor = Palette.barControl
        cancelButton.layer.cornerRadius = 22
        cancelButton.accessibilityLabel = "Discard dictation"
        cancelButton.addTarget(self, action: #selector(cancelTapped), for: .touchUpInside)
        cancelButton.isHidden = true

        // The pill: the cleanup level, cycled by a tap.
        var pillConfiguration = UIButton.Configuration.filled()
        pillConfiguration.baseBackgroundColor = Palette.pill
        pillConfiguration.baseForegroundColor = Palette.pillText
        pillConfiguration.cornerStyle = .capsule
        pillConfiguration.contentInsets = NSDirectionalEdgeInsets(top: 0, leading: 24, bottom: 0, trailing: 24)
        pill.configuration = pillConfiguration
        pill.addTarget(self, action: #selector(pillTapped), for: .touchUpInside)
        pill.accessibilityHint = "Cycles the cleanup level"

        // Mic, or ✓ while listening.
        rightButton.backgroundColor = Palette.pill
        rightButton.tintColor = Palette.pillText
        rightButton.layer.cornerRadius = 22
        rightButton.addTarget(self, action: #selector(rightTapped), for: .touchUpInside)

        spinner.color = Palette.pillText
        spinner.hidesWhenStopped = true
        rightButton.addSubview(spinner)

        for view in [menuButton, cancelButton, pill, rightButton] {
            view.translatesAutoresizingMaskIntoConstraints = false
            addSubview(view)
        }
        spinner.translatesAutoresizingMaskIntoConstraints = false

        NSLayoutConstraint.activate([
            menuButton.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 14),
            menuButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            menuButton.widthAnchor.constraint(equalToConstant: 44),
            menuButton.heightAnchor.constraint(equalToConstant: 44),

            cancelButton.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 12),
            cancelButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            cancelButton.widthAnchor.constraint(equalToConstant: 44),
            cancelButton.heightAnchor.constraint(equalToConstant: 44),

            rightButton.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            rightButton.centerYAnchor.constraint(equalTo: centerYAnchor),
            rightButton.widthAnchor.constraint(equalToConstant: 44),
            rightButton.heightAnchor.constraint(equalToConstant: 44),

            pill.trailingAnchor.constraint(equalTo: rightButton.leadingAnchor, constant: -6),
            pill.centerYAnchor.constraint(equalTo: centerYAnchor),
            pill.heightAnchor.constraint(equalToConstant: 44),
            pill.widthAnchor.constraint(greaterThanOrEqualToConstant: 112),

            spinner.centerXAnchor.constraint(equalTo: rightButton.centerXAnchor),
            spinner.centerYAnchor.constraint(equalTo: rightButton.centerYAnchor),
        ])

        setLevel(.light)
        apply(.typing)
    }

    // MARK: - State

    func setLevel(_ level: CleanupLevel) {
        self.level = level
        var configuration = pill.configuration
        configuration?.attributedTitle = AttributedString(
            level.label,
            attributes: AttributeContainer([
                .font: UIFont.systemFont(ofSize: 18, weight: .medium),
            ])
        )
        pill.configuration = configuration
        pill.accessibilityLabel = "Cleanup: \(level.label)"
    }

    /// Whether the menu offers "Release the mic" — only meaningful while the app is
    /// alive in standby, which is the only time there is a mic to release.
    func setCanRelease(_ can: Bool) {
        canRelease = can
    }

    func setMode(_ mode: Mode) {
        guard mode != self.mode else { return }
        self.mode = mode
        UIView.transition(with: self, duration: 0.18, options: [.transitionCrossDissolve, .allowUserInteraction]) {
            self.apply(mode)
        }
    }

    private func apply(_ mode: Mode) {
        let listening = mode == .listening
        let busy = mode == .transcribing
        let notice = mode == .notice
        menuButton.isHidden = listening || busy || notice
        cancelButton.isHidden = !(listening || notice)
        // The same round control on the left does two jobs: ✕ discards a dictation,
        // and a keyboard glyph brings the keys back from under a notice.
        cancelButton.setImage(
            UIImage(systemName: notice ? "keyboard" : "xmark",
                    withConfiguration: UIImage.SymbolConfiguration(pointSize: notice ? 16 : 15, weight: .semibold)),
            for: .normal
        )
        cancelButton.accessibilityLabel = notice ? "Back to the keyboard" : "Discard dictation"
        pill.alpha = (listening || busy) ? 0.35 : 1
        pill.isUserInteractionEnabled = !(listening || busy)

        if busy {
            rightButton.setImage(nil, for: .normal)
            rightButton.isUserInteractionEnabled = false
            rightButton.accessibilityLabel = "Transcribing"
            spinner.startAnimating()
        } else {
            spinner.stopAnimating()
            rightButton.isUserInteractionEnabled = true
            let symbol = listening ? "checkmark" : "mic.fill"
            rightButton.setImage(
                UIImage(systemName: symbol,
                        withConfiguration: UIImage.SymbolConfiguration(pointSize: 19, weight: .semibold)),
                for: .normal
            )
            rightButton.accessibilityLabel = listening ? "Finish dictation" : "Dictate"
        }
    }

    /// Repaint the colours that do not follow the trait collection on their own.
    func repaint() {
        cancelButton.backgroundColor = Palette.barControl
        rightButton.backgroundColor = Palette.pill
        rightButton.tintColor = Palette.pillText
        menuButton.tintColor = Palette.textPrimary
        var configuration = pill.configuration
        configuration?.baseBackgroundColor = Palette.pill
        configuration?.baseForegroundColor = Palette.pillText
        pill.configuration = configuration
    }

    // MARK: - Menu

    /// Two items, deliberately. A context menu inside a keyboard extension is
    /// confined to the keyboard's own window, which is about 270 pt tall, and a menu
    /// with a level list in it was cut off below the second level. The level has
    /// the pill; this holds only what has nowhere else to live.
    private func menuItems() -> [UIMenuElement] {
        var items: [UIMenuElement] = [
            UIAction(title: "Open WhimprFlow", image: UIImage(systemName: "gearshape")) { [weak self] _ in
                self?.onOpenApp?()
            },
        ]
        if canRelease {
            // The orange indicator is seen from other apps; the answer to "why is
            // that on" should be one tap from wherever it is asked.
            items.append(UIAction(title: "Release the mic", image: UIImage(systemName: "mic.slash"),
                                  attributes: .destructive) { [weak self] _ in
                self?.onRelease?()
            })
        }
        return items
    }

    // MARK: - Actions

    @objc private func pillTapped() { onPill?() }

    @objc private func cancelTapped() {
        if mode == .notice { onDismissNotice?() } else { onCancel?() }
    }

    @objc private func rightTapped() {
        switch mode {
        case .typing, .notice: onMic?()
        case .listening: onConfirm?()
        case .transcribing: break
        }
    }
}
