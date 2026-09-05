import UIKit

/// The WhimprFlow keyboard.
///
/// # What this can and cannot do
///
/// It **cannot record audio**. App extensions have no microphone entitlement and iOS
/// refuses the capture; `RequestsOpenAccess` does not change that. So the mic key asks
/// the container app to record, and this class waits for text to appear in the shared
/// container and inserts it at the cursor.
///
/// Two ways to ask, and the choice is made per tap:
///
/// - The app is alive (its heartbeat is recent, because it holds a capture session in
///   standby): post a Darwin notification. Nothing visibly switches.
/// - It is not: open `whimprflow://dictate`. iOS shows a back arrow to return here.
///
/// # Getting to another keyboard
///
/// A custom keyboard cannot select a *specific* system keyboard — there is no API to
/// jump to Emoji or to the English layout. What exists is `advanceToNextInputMode()`,
/// which cycles, and `handleInputModeList(from:with:)`, which raises the system
/// picker. The globe key does the first on a tap and the second on a long press,
/// matching what the stock keyboard does, so "give me ABC" and "give me emoji" are
/// both one gesture away even though neither can be a dedicated button.
final class KeyboardViewController: UIInputViewController {
    private var micButton: UIButton!
    private var micLabel: UILabel!
    private var waveform: WaveformView!
    private var lastInsertedResultID = 0
    private var isDictating = false

    /// A keyboard has no intrinsic height, and the stock one is around this on a
    /// phone. Tall enough for the mic target plus one row of keys.
    private let keyboardHeight: CGFloat = 258

    // MARK: - Lifecycle

    override func viewDidLoad() {
        super.viewDidLoad()
        buildInterface()
        applyAppearance()
        // Anything already in the container predates this keyboard appearing and must
        // not be inserted — otherwise switching to the keyboard replays the last
        // dictation into whatever field happens to be focused.
        lastInsertedResultID = Handoff.latestResult()?.id ?? 0
        observeHandoff()
        refresh()
    }

    deinit {
        Handoff.stopObserving(observer: Unmanaged.passUnretained(self).toOpaque())
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        applyAppearance()
        refresh()
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        // The display link keeps firing on a keyboard the user has switched away from
        // otherwise, which is wasted work in the tightest process budget on the phone.
        waveform.stop()
    }

    override func traitCollectionDidChange(_ previous: UITraitCollection?) {
        super.traitCollectionDidChange(previous)
        // Dynamic colours re-resolve themselves; the layer-backed bits do not.
        micButton.backgroundColor = Palette.surface.resolvedColor(with: traitCollection)
    }

    /// Follow the app's appearance setting, which the keyboard reads from the shared
    /// container. `.unspecified` for System, so it tracks the device rather than being
    /// frozen at whatever it was when the keyboard was built.
    private func applyAppearance() {
        overrideUserInterfaceStyle = Settings.storedAppearance.interfaceStyle
        view.backgroundColor = Palette.background
    }

    // MARK: - Interface

    private func buildInterface() {
        view.backgroundColor = Palette.background

        // The mic target: everything above the key row, so it is hard to miss.
        micButton = UIButton(type: .custom)
        micButton.backgroundColor = Palette.surface
        micButton.layer.cornerRadius = 16
        micButton.layer.cornerCurve = .continuous
        micButton.layer.borderWidth = 1
        micButton.layer.borderColor = Palette.border.cgColor
        micButton.accessibilityLabel = "Dictate"
        micButton.addTarget(self, action: #selector(micTapped), for: .touchUpInside)
        micButton.translatesAutoresizingMaskIntoConstraints = false

        waveform = WaveformView()
        waveform.translatesAutoresizingMaskIntoConstraints = false
        waveform.isUserInteractionEnabled = false
        micButton.addSubview(waveform)

        micLabel = UILabel()
        micLabel.font = .preferredFont(forTextStyle: .footnote)
        micLabel.textColor = Palette.textSecondary
        micLabel.textAlignment = .center
        micLabel.numberOfLines = 2
        micLabel.adjustsFontSizeToFitWidth = true
        micLabel.minimumScaleFactor = 0.85
        micLabel.translatesAutoresizingMaskIntoConstraints = false
        micButton.addSubview(micLabel)

        // The bottom row, in the stock keyboard's proportions: the two glyph keys
        // narrow at the edges, return a little wider, and space taking everything
        // that is left. Equal widths made every key look like a modifier and the
        // spacebar impossible to hit by feel.
        let globe = key(symbol: "globe", action: #selector(globeTapped), accessibility: "Next keyboard")
        globe.addGestureRecognizer(
            UILongPressGestureRecognizer(target: self, action: #selector(globeLongPressed))
        )
        let delete = key(symbol: "delete.left", action: #selector(deleteTapped), accessibility: "Delete")
        let space = key(title: "space", action: #selector(spaceTapped), accessibility: "Space")
        let ret = key(symbol: "return", action: #selector(returnTapped), accessibility: "Return")

        let row = UIStackView(arrangedSubviews: [globe, delete, space, ret])
        row.axis = .horizontal
        row.spacing = 6
        row.distribution = .fill
        row.translatesAutoresizingMaskIntoConstraints = false

        view.addSubview(micButton)
        view.addSubview(row)

        NSLayoutConstraint.activate([
            view.heightAnchor.constraint(equalToConstant: keyboardHeight),

            micButton.topAnchor.constraint(equalTo: view.topAnchor, constant: 8),
            micButton.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 6),
            micButton.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -6),

            waveform.centerXAnchor.constraint(equalTo: micButton.centerXAnchor),
            waveform.centerYAnchor.constraint(equalTo: micButton.centerYAnchor, constant: -8),
            waveform.widthAnchor.constraint(equalToConstant: 120),
            waveform.heightAnchor.constraint(equalToConstant: 46),

            micLabel.leadingAnchor.constraint(equalTo: micButton.leadingAnchor, constant: 12),
            micLabel.trailingAnchor.constraint(equalTo: micButton.trailingAnchor, constant: -12),
            micLabel.bottomAnchor.constraint(equalTo: micButton.bottomAnchor, constant: -10),

            row.topAnchor.constraint(equalTo: micButton.bottomAnchor, constant: 8),
            row.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 6),
            row.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -6),
            row.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -6),
            // 46 clears Apple's 44pt minimum target with a little room.
            row.heightAnchor.constraint(equalToConstant: 46),

            globe.widthAnchor.constraint(equalToConstant: 46),
            delete.widthAnchor.constraint(equalToConstant: 46),
            ret.widthAnchor.constraint(equalToConstant: 74),
        ])
    }

    private func key(
        symbol: String? = nil,
        title: String? = nil,
        action: Selector,
        accessibility: String
    ) -> UIButton {
        var configuration = UIButton.Configuration.plain()
        if let symbol { configuration.image = UIImage(systemName: symbol) }
        if let title {
            configuration.title = title
            configuration.attributedTitle = AttributedString(
                title,
                attributes: AttributeContainer([.font: UIFont.preferredFont(forTextStyle: .body)])
            )
        }
        configuration.baseForegroundColor = Palette.textPrimary

        let button = UIButton(configuration: configuration)
        button.backgroundColor = Palette.control
        button.layer.cornerRadius = 8
        button.layer.cornerCurve = .continuous
        // The stock keyboard's key shadow, which is most of why these read as keys.
        button.layer.shadowColor = UIColor.black.cgColor
        button.layer.shadowOpacity = 0.28
        button.layer.shadowOffset = CGSize(width: 0, height: 1)
        button.layer.shadowRadius = 0
        button.accessibilityLabel = accessibility
        button.addTarget(self, action: action, for: .touchUpInside)
        return button
    }

    // MARK: - Actions

    @objc private func micTapped() {
        // Without Full Access there is no container and no network; say so rather
        // than appearing to work.
        guard Handoff.isReachable else {
            micLabel.text = "Turn on Allow Full Access in Settings to dictate."
            return
        }
        if isDictating {
            Handoff.post(.stop)
            return
        }
        if Handoff.isAppLive {
            Handoff.post(.start)
        } else {
            openContainerApp()
        }
    }

    /// Tap: the next keyboard, which is normally the letters one. Long press: the
    /// system picker, which is the only route to a *specific* keyboard such as Emoji.
    @objc private func globeTapped() {
        advanceToNextInputMode()
    }

    @objc private func globeLongPressed(_ gesture: UILongPressGestureRecognizer) {
        guard gesture.state == .began else { return }
        handleInputModeList(from: gesture.view ?? view, with: UIEvent())
    }

    @objc private func deleteTapped() { textDocumentProxy.deleteBackward() }
    @objc private func spaceTapped() { textDocumentProxy.insertText(" ") }
    @objc private func returnTapped() { textDocumentProxy.insertText("\n") }

    /// Launching the *container* app is the one exception to the App Review rule that
    /// a keyboard "must not launch other apps", confirmed by Apple DTS. Since iOS 26
    /// it also requires Full Access, which is checked before this is reached.
    ///
    /// The responder-chain walk is the documented way: an extension has no
    /// `UIApplication.shared`.
    private func openContainerApp() {
        var responder: UIResponder? = self
        while let current = responder {
            if let application = current as? UIApplication {
                application.open(Handoff.dictateURL)
                return
            }
            responder = current.next
        }
        micLabel.text = "Could not open WhimprFlow."
    }

    // MARK: - Handoff

    private func observeHandoff() {
        let observer = Unmanaged.passUnretained(self).toOpaque()
        let callback: CFNotificationCallback = { _, observer, _, _, _ in
            guard let observer else { return }
            let controller = Unmanaged<KeyboardViewController>
                .fromOpaque(observer).takeUnretainedValue()
            DispatchQueue.main.async { controller.refresh() }
        }
        for signal: Handoff.Signal in [.result, .state, .alive] {
            Handoff.observe(signal, observer: observer, callback: callback)
        }
    }

    /// Insert anything new, and reflect the app's state in the key.
    private func refresh() {
        let state = Handoff.state
        isDictating = (state == .recording)

        if let result = Handoff.latestResult(), result.id > lastInsertedResultID {
            lastInsertedResultID = result.id
            textDocumentProxy.insertText(result.text)
        }

        switch state {
        case .recording:
            setMic(symbol: nil, label: "Listening — tap to finish")
            waveform.isHidden = false
            waveform.start()
        case .transcribing:
            waveform.stop()
            waveform.isHidden = true
            setMic(symbol: "waveform", label: "Transcribing…")
        case .failed:
            waveform.stop()
            waveform.isHidden = true
            setMic(symbol: "exclamationmark.triangle", label: "Dictation failed — open WhimprFlow")
        case .idle:
            waveform.stop()
            waveform.isHidden = true
            setMic(
                symbol: "mic.fill",
                label: Handoff.isReachable
                    ? "Tap to dictate"
                    : "Turn on Allow Full Access in Settings to dictate."
            )
        }
    }

    /// The glyph and the caption. A nil symbol means the waveform has the space
    /// instead — showing both put a static mic above moving bars, which read as two
    /// unrelated things happening.
    private func setMic(symbol: String?, label: String) {
        var configuration = UIButton.Configuration.plain()
        if let symbol {
            configuration.image = UIImage(
                systemName: symbol,
                withConfiguration: UIImage.SymbolConfiguration(pointSize: 30, weight: .medium)
            )
        }
        configuration.baseForegroundColor = isDictating ? Palette.accent : Palette.textPrimary
        micButton.configuration = configuration
        micButton.backgroundColor = Palette.surface
        micLabel.text = label
    }
}
