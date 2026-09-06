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
/// - It is not: open `whimprflow://dictate`. You see the app, then come back.
///
/// # Shape
///
/// A top bar — menu, level pill, mic — over a full typing keyboard, and while a
/// dictation is happening the key area becomes the listening screen. This is Wispr
/// Flow's arrangement, adopted deliberately: it is what people who dictate on a phone
/// already know. What the four screens do is split across `TopBar`, `KeyboardView`,
/// `ListeningView` and `TypingEngine`; this class only connects them to the host
/// field and to the app.
///
/// # Getting to another keyboard
///
/// A custom keyboard cannot select a *specific* system keyboard — there is no API to
/// jump to Emoji or to the English layout. What exists is `advanceToNextInputMode()`,
/// which cycles, and `handleInputModeList(from:with:)`, which raises the system
/// picker. On Face ID phones iOS draws the globe itself, below the keyboard, and
/// `needsInputModeSwitchKey` is false; a globe key is drawn here only when it is true.
final class KeyboardViewController: UIInputViewController {
    /// Our own backdrop, sized to the keyboard and hung from the bottom. The root
    /// view itself stays transparent — see `buildInterface`.
    private var backdrop: UIView!
    private var topBar: TopBar!
    private var keyboardView: KeyboardView!
    private var listeningView: ListeningView!
    private var engine: TypingEngine!
    private let decoder = SwipeDecoder()
    private let haptic = UIImpactFeedbackGenerator(style: .light)

    private var lastInsertedResultID = 0
    private var isDictating = false
    /// The app state seen on the previous refresh. A `failed` is shown only when it
    /// follows a recording or transcribing this keyboard watched: the app leaves the
    /// last outcome in the container, and a failure from hours ago is not what
    /// someone opening the keyboard wants to read.
    private var lastObservedState: Handoff.State = .idle
    /// The screen the key area is showing.
    private enum Screen: Equatable { case typing, listening, transcribing, failed }
    private var screen: Screen = .typing

    /// What `overrideUserInterfaceStyle` was last set to, so it is only written when
    /// it actually changes — assigning it re-resolves every dynamic colour and
    /// re-renders the whole keyboard, which is visible as a flash if done on each
    /// appearance.
    private var appliedStyle: UIUserInterfaceStyle?

    /// The keyboard's height: the bar plus the key grid, from `KeyboardView.Metrics`
    /// for the device and width. Installed in `viewDidLoad` so the very first layout
    /// is already this tall.
    ///
    /// Taller than the stock keyboard's 242 by the height of the bar, as Wispr Flow's
    /// is. The keyboard-switch frame that a mismatched height causes is handled by
    /// the layout, not the number: for one frame per switch iOS lays this view out at
    /// the *outgoing* keyboard's height (seen frame-by-frame in a device screen
    /// recording), so everything hangs from the bottom with a fixed panel height and
    /// nothing is pinned to the top. A top-pinned, flexible layout fills that frame by
    /// growing upward over the host's text field and snaps back the next frame.
    ///
    /// Not left to the system: without a constraint iOS hands the extension almost no
    /// height and the panel collapses. Not installed later, in
    /// `updateViewConstraints`: the keyboard then appears at whatever it was given and
    /// jumps. Priority 999 so it yields to the system's transient height during a
    /// switch rather than conflicting; 1000 was tried and changed nothing.
    /// On the iPad the grid is taller in landscape than in portrait, so these are
    /// re-set from `viewWillLayoutSubviews` whenever the width changes class.
    private var heightConstraint: NSLayoutConstraint!
    private var backdropHeightConstraint: NSLayoutConstraint!
    private var keysHeightConstraint: NSLayoutConstraint!
    private var appliedMetrics: KeyboardView.Metrics?

    // MARK: - Lifecycle

    override func viewDidLoad() {
        super.viewDidLoad()
        engine = TypingEngine(proxy: textDocumentProxy)
        engine.onCorrection = { [weak self] correction in
            self?.keyboardView.showHint("\(correction.from) → \(correction.to)")
        }
        buildInterface()
        applyAppearance()
        // Anything already in the container predates this keyboard appearing and must
        // not be inserted — otherwise switching to the keyboard replays the last
        // dictation into whatever field happens to be focused.
        lastInsertedResultID = Handoff.latestResult()?.id ?? 0
        observeHandoff()
        syncTyping()
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
        topBar.setLevel(Settings.storedLevel)
        engine.autocorrectEnabled = Settings.storedAutocorrect
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        // `needsInputModeSwitchKey` is only meaningful once the view is in a window.
        keyboardView.includeGlobe = needsInputModeSwitchKey
        // Syncing state *after* the transition rather than during it: `refresh()`
        // touches several views, and doing that while the keyboard is sliding in is
        // the jitter seen when switching back to it.
        refresh()
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        // The display link and the clock keep firing on a keyboard the user has
        // switched away from otherwise, which is wasted work in the tightest process
        // budget on the phone.
        listeningView.pause()
    }

    override func viewWillLayoutSubviews() {
        super.viewWillLayoutSubviews()
        applyMetrics(for: view.bounds.width)
    }

    /// Size the grid and the panel for this width. Only on a real change: the
    /// constraint constants are what iOS animates a keyboard switch against.
    private func applyMetrics(for width: CGFloat) {
        guard width > 0 else { return }
        let metrics = KeyboardView.Metrics.for(width: width)
        guard metrics != appliedMetrics else { return }
        appliedMetrics = metrics
        keyboardView.metrics = metrics
        keysHeightConstraint.constant = metrics.height
        heightConstraint.constant = TopBar.height + metrics.height
        backdropHeightConstraint.constant = TopBar.height + metrics.height
    }

    override func textDidChange(_ textInput: UITextInput?) {
        super.textDidChange(textInput)
        // The host moved the cursor, cleared the field, or focused another one. The
        // sentence rule and the return key both depend on where we now are.
        syncTyping()
    }

    override func traitCollectionDidChange(_ previous: UITraitCollection?) {
        super.traitCollectionDidChange(previous)
        // This also fires during transitions, so only act when light/dark genuinely
        // flipped. Dynamic colours re-resolve themselves; CGColor on a layer does not.
        guard previous?.userInterfaceStyle != traitCollection.userInterfaceStyle else { return }
        keyboardView.repaint()
        topBar.repaint()
    }

    /// Follow the app's appearance setting, which the keyboard reads from the shared
    /// container. `.unspecified` for System, so it tracks the device rather than being
    /// frozen at whatever it was when the keyboard was built.
    private func applyAppearance() {
        let style = Settings.storedAppearance.interfaceStyle
        guard style != appliedStyle else { return }
        appliedStyle = style
        overrideUserInterfaceStyle = style
        backdrop?.backgroundColor = Palette.keyboardBackdrop
    }

    // MARK: - Interface

    private func buildInterface() {
        // Transparent, on purpose. For one frame during a keyboard switch iOS lays
        // this view out at the *outgoing* keyboard's height; bottom-anchoring the
        // content stopped that frame moving anything, but the extra height was
        // still filled with an opaque near-black background — painted over the host
        // app's text field for that frame, which a device recording shows as a
        // black flash above the keyboard. With the root clear, the transient extra
        // area shows the host through, and the backdrop is drawn only behind the
        // content, whose position never changes.
        view.backgroundColor = .clear

        backdrop = UIView()
        backdrop.backgroundColor = Palette.keyboardBackdrop

        topBar = TopBar()
        topBar.onOpenApp = { [weak self] in self?.openContainerApp(Handoff.settingsURL) }
        topBar.onRelease = { [weak self] in self?.releaseMic() }
        topBar.onPill = { [weak self] in
            guard let self else { return }
            self.feedback()
            self.setLevel(Settings.storedLevel.next)
        }
        topBar.onMic = { [weak self] in self?.micTapped() }
        topBar.onCancel = { [weak self] in self?.discardTapped() }
        topBar.onDismissNotice = { [weak self] in self?.show(.typing) }
        topBar.onConfirm = { [weak self] in self?.finishTapped() }

        keyboardView = KeyboardView()
        keyboardView.delegate = self

        listeningView = ListeningView()
        listeningView.isHidden = true
        listeningView.onRetry = { [weak self] in self?.micTapped() }
        listeningView.onOpenApp = { [weak self] in self?.openContainerApp(Handoff.dictateURL) }

        for subview in [backdrop, topBar, keyboardView, listeningView] as [UIView] {
            subview.translatesAutoresizingMaskIntoConstraints = false
            view.addSubview(subview)
        }

        let initial = KeyboardView.Metrics.for(width: UIScreen.main.bounds.width)
        appliedMetrics = initial
        keyboardView.metrics = initial
        heightConstraint = view.heightAnchor.constraint(equalToConstant: TopBar.height + initial.height)
        heightConstraint.priority = UILayoutPriority(999)
        backdropHeightConstraint = backdrop.heightAnchor.constraint(equalToConstant: TopBar.height + initial.height)
        keysHeightConstraint = keyboardView.heightAnchor.constraint(equalToConstant: initial.height)

        NSLayoutConstraint.activate([
            heightConstraint,
            backdrop.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            backdrop.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            backdrop.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            backdropHeightConstraint,

            // Everything hangs from the bottom — see the note on the height above.
            keyboardView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            keyboardView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            keyboardView.bottomAnchor.constraint(equalTo: view.bottomAnchor),
            keysHeightConstraint,

            listeningView.leadingAnchor.constraint(equalTo: keyboardView.leadingAnchor),
            listeningView.trailingAnchor.constraint(equalTo: keyboardView.trailingAnchor),
            listeningView.topAnchor.constraint(equalTo: keyboardView.topAnchor),
            listeningView.bottomAnchor.constraint(equalTo: keyboardView.bottomAnchor),

            topBar.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            topBar.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            topBar.bottomAnchor.constraint(equalTo: keyboardView.topAnchor),
            topBar.heightAnchor.constraint(equalToConstant: TopBar.height),
        ])
    }

    /// Show one of the key-area screens. The bar follows.
    private func show(_ next: Screen) {
        guard next != screen else { return }
        screen = next
        let typing = next == .typing
        UIView.transition(with: view, duration: 0.18, options: [.transitionCrossDissolve, .allowUserInteraction]) {
            self.keyboardView.isHidden = !typing
            self.listeningView.isHidden = typing
        }
        switch next {
        case .typing: topBar.setMode(.typing)
        case .failed: topBar.setMode(.notice)
        case .listening: topBar.setMode(.listening)
        case .transcribing: topBar.setMode(.transcribing)
        }
    }

    // MARK: - Typing

    /// Push the engine's view of the world into the grid.
    private func syncTyping() {
        engine.refreshShift()
        keyboardView.plane = engine.plane
        keyboardView.shift = engine.shift
        keyboardView.returnTitle = engine.returnKeyTitle
    }

    /// Key click and a light tap. Both are no-ops without Full Access — the sound
    /// needs the system's audio, the haptic the system's engine — so they are not
    /// attempted then, rather than failing quietly on every key.
    private func feedback() {
        guard Handoff.isReachable else { return }
        UIDevice.current.playInputClick()
        haptic.impactOccurred(intensity: 0.6)
    }

    // MARK: - Level

    private func setLevel(_ level: CleanupLevel) {
        Settings.storedLevel = level
        topBar.setLevel(level)
        // The app caches the level; tell it to look again before the next dictation.
        Handoff.post(.settings)
    }

    // MARK: - Dictation

    private func micTapped() {
        // Without Full Access there is no container and no network; say so rather
        // than appearing to work.
        guard Handoff.isReachable else {
            listeningView.setMode(.failed(
                "Turn on Allow Full Access for WhimprFlow in Settings → General → Keyboard to dictate.",
                canRetry: false
            ))
            show(.failed)
            return
        }
        feedback()
        if isDictating {
            Handoff.post(.stop)
            return
        }
        if Handoff.isAppLive {
            Handoff.post(.start)
        } else {
            // Say why. iOS is about to switch apps, and without a reason that reads as
            // the keyboard malfunctioning rather than as the one thing it cannot avoid:
            // a terminated or timed-out app cannot be woken by a notification, and no
            // extension can launch its container app in the background.
            listeningView.setMode(.failed("Waking the mic — opening WhimprFlow. Swipe back when it says ready.", canRetry: false))
            show(.failed)
            openContainerApp(Handoff.dictateURL)
        }
    }

    private func finishTapped() {
        guard Handoff.isReachable else { return }
        feedback()
        Handoff.post(.stop)
    }

    private func discardTapped() {
        guard Handoff.isReachable else { return }
        feedback()
        Handoff.post(.cancel)
    }

    private func releaseMic() {
        guard Handoff.isReachable else { return }
        Handoff.post(.release)
    }

    /// Launching the *container* app is the one exception to the App Review rule that
    /// a keyboard "must not launch other apps", confirmed by Apple DTS. Since iOS 26
    /// it also requires Full Access, which is checked before this is reached.
    ///
    /// The responder-chain walk is the documented way: an extension has no
    /// `UIApplication.shared`.
    private func openContainerApp(_ url: URL) {
        var responder: UIResponder? = self
        while let current = responder {
            if let application = current as? UIApplication {
                application.open(url)
                return
            }
            responder = current.next
        }
        listeningView.setMode(.failed("Could not open WhimprFlow.", canRetry: false))
        show(.failed)
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

    /// Insert anything new, and reflect the app's state on screen.
    private func refresh() {
        let state = Handoff.state
        isDictating = (state == .recording)
        topBar.setCanRelease(Handoff.isAppLive)

        if let result = Handoff.latestResult(), result.id > lastInsertedResultID {
            lastInsertedResultID = result.id
            engine.insertDictation(result.text)
            syncTyping()
        }

        switch state {
        case .recording:
            listeningView.setMode(.listening)
            show(.listening)
        case .transcribing:
            listeningView.setMode(.transcribing)
            show(.transcribing)
        case .failed:
            if lastObservedState == .recording || lastObservedState == .transcribing {
                listeningView.setMode(.failed(
                    "Recognition or cleanup did not answer. Try again, or open WhimprFlow to see why.",
                    canRetry: true
                ))
                show(.failed)
            } else if screen == .listening || screen == .transcribing {
                show(.typing)
            }
        case .idle:
            // A notice we put up ourselves (no Full Access, opening the app) stays
            // until the user acts; the app's idle is not news about it.
            if screen != .failed { show(.typing) }
        }
        lastObservedState = state
    }
}

// MARK: - Keys

extension KeyboardViewController: KeyboardViewDelegate {
    func keyboardViewDidTouchKey(_ view: KeyboardView) {
        feedback()
    }

    func keyboardView(_ view: KeyboardView, didCommit key: Key) {
        // Any keypress ends a notice; the user has moved on.
        if screen == .failed { show(.typing) }
        if key != .shift { topBar.hideAlternatives() }
        switch key {
        case .globe:
            advanceToNextInputMode()
        case .hide:
            dismissKeyboard()
        case .dictate:
            micTapped()
        default:
            if engine.commit(key) { syncTyping() }
        }
    }

    func keyboardViewDidLongPressGlobe(_ view: KeyboardView) {
        handleInputModeList(from: view, with: UIEvent())
    }

    func keyboardView(_ view: KeyboardView, moveCursorBy offset: Int) {
        textDocumentProxy.adjustTextPosition(byCharacterOffset: offset)
        // Moving into or out of a word changes what shift and autocorrect should do.
        syncTyping()
    }

    func keyboardView(_ view: KeyboardView, didSwipe path: [CGPoint]) {
        if screen == .failed { show(.typing) }
        let candidates = decoder.decode(path: path, centres: view.letterCentres(), keyWidth: view.letterKeyWidth)
        guard let best = candidates.first else { return }
        engine.insertSwipe(best.word)
        syncTyping()
        feedback()
        let others = candidates.dropFirst().map(\.word)
        if others.isEmpty {
            topBar.hideAlternatives()
        } else {
            topBar.showAlternatives(Array(others)) { [weak self] word in
                self?.engine.replaceLastSwipe(with: word)
            }
        }
    }
}
