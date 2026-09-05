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
    private var discardButton: UIButton!
    private var waveform: WaveformView!
    private var lastInsertedResultID = 0
    private var isDictating = false

    /// What `overrideUserInterfaceStyle` was last set to, so it is only written when
    /// it actually changes — assigning it re-resolves every dynamic colour and
    /// re-renders the whole keyboard, which is visible as a flash if done on each
    /// appearance.
    private var appliedStyle: UIUserInterfaceStyle?

    /// The glyph currently on the mic key, so it is only rebuilt on a real change.
    /// `.some(nil)` is a real value here — it means the waveform has the space —
    /// hence the double optional rather than a plain String?.
    private var appliedMicSymbol: String??

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
        // Appearance only. It is guarded to a no-op unless the setting changed, and
        // it must be right before the first frame is drawn.
        applyAppearance()
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        // Syncing state *after* the transition rather than during it. `refresh()`
        // touches the mic key, the waveform and the discard button, and doing that
        // while the keyboard is sliding in is the jitter seen when switching back to
        // it. Nothing here is urgent enough to be worth a frame of the animation.
        refresh()
    }

    // No height constraint, on purpose.
    //
    // Every keyboard switch animates the keyboard frame to the incoming keyboard's
    // height. A keyboard that declares its own height therefore moves the top edge on
    // every switch in both directions — which is the "flicker up top" — and if the
    // constraint lands after the first layout, the keyboard also appears at the system
    // height and then jumps, on every relaunch of the extension. Taking the height iOS
    // gives every keyboard means the edge never moves. The layout below is flexible:
    // the mic panel absorbs whatever is left after the key row.
    //
    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        // The display link keeps firing on a keyboard the user has switched away from
        // otherwise, which is wasted work in the tightest process budget on the phone.
        waveform.stop()
    }

    override func traitCollectionDidChange(_ previous: UITraitCollection?) {
        super.traitCollectionDidChange(previous)
        // This also fires during transitions, so only act when light/dark genuinely
        // flipped. Dynamic colours re-resolve themselves; CGColor on a layer does not,
        // which is why the border is repainted by hand.
        guard previous?.userInterfaceStyle != traitCollection.userInterfaceStyle else { return }
        micButton.backgroundColor = Palette.surface.resolvedColor(with: traitCollection)
        micButton.layer.borderColor = Palette.border.resolvedColor(with: traitCollection).cgColor
    }

    /// Follow the app's appearance setting, which the keyboard reads from the shared
    /// container. `.unspecified` for System, so it tracks the device rather than being
    /// frozen at whatever it was when the keyboard was built.
    private func applyAppearance() {
        let style = Settings.storedAppearance.interfaceStyle
        // Only on an actual change. Writing this unconditionally on every appearance
        // re-resolves every dynamic colour and re-renders the keyboard, which is one
        // of the flashes seen when switching back to it.
        guard style != appliedStyle else { return }
        appliedStyle = style
        overrideUserInterfaceStyle = style
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

        // Discard. Only while recording, and its own control rather than a gesture on
        // the panel: stopping and throwing away are both one tap, and the difference
        // between them is a recognition call plus a cleanup call on audio the user has
        // already decided against.
        discardButton = UIButton(type: .system)
        discardButton.setImage(
            UIImage(
                systemName: "xmark",
                withConfiguration: UIImage.SymbolConfiguration(pointSize: 14, weight: .semibold)
            ),
            for: .normal
        )
        discardButton.tintColor = Palette.textSecondary
        discardButton.backgroundColor = Palette.control
        discardButton.layer.cornerRadius = 16
        discardButton.layer.cornerCurve = .continuous
        discardButton.accessibilityLabel = "Discard dictation"
        discardButton.isHidden = true
        discardButton.translatesAutoresizingMaskIntoConstraints = false
        discardButton.addTarget(self, action: #selector(discardTapped), for: .touchUpInside)
        micButton.addSubview(discardButton)

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

            discardButton.topAnchor.constraint(equalTo: micButton.topAnchor, constant: 10),
            discardButton.trailingAnchor.constraint(equalTo: micButton.trailingAnchor, constant: -10),
            discardButton.widthAnchor.constraint(equalToConstant: 32),
            discardButton.heightAnchor.constraint(equalToConstant: 32),

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
            // Say why. iOS is about to switch apps, and without a reason that reads as
            // the keyboard malfunctioning rather than as the one thing it cannot avoid:
            // a terminated app cannot be woken by a notification, and no extension can
            // launch its container app in the background.
            micLabel.text = "WhimprFlow isn't running — opening it"
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

    @objc private func discardTapped() {
        guard Handoff.isReachable else { return }
        Handoff.post(.cancel)
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
            discardButton.isHidden = false
        case .transcribing:
            discardButton.isHidden = true
            waveform.stop()
            waveform.isHidden = true
            setMic(symbol: "waveform", label: "Transcribing…")
        case .failed:
            discardButton.isHidden = true
            waveform.stop()
            waveform.isHidden = true
            setMic(symbol: "exclamationmark.triangle", label: "Dictation failed — open WhimprFlow")
        case .idle:
            discardButton.isHidden = true
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
        // Only when it actually changed. Assigning `configuration` forces the button
        // to rebuild and re-lay-out its contents, and `refresh()` runs on every
        // appearance — so doing this unconditionally rebuilds the mic key in the
        // middle of the keyboard's presentation transition, every single time, for a
        // state that is usually identical to the last one.
        if symbol != appliedMicSymbol {
            appliedMicSymbol = symbol
            var configuration = UIButton.Configuration.plain()
            if let symbol {
                configuration.image = UIImage(
                    systemName: symbol,
                    withConfiguration: UIImage.SymbolConfiguration(pointSize: 30, weight: .medium)
                )
            }
            configuration.baseForegroundColor = isDictating ? Palette.accent : Palette.textPrimary
            micButton.configuration = configuration
        }
        if micLabel.text != label { micLabel.text = label }
    }
}
