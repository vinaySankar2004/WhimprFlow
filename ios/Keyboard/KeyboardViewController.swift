import UIKit

/// The WhimprFlow keyboard.
///
/// # What this can and cannot do
///
/// It **cannot record audio**. App extensions have no microphone entitlement and iOS
/// refuses the capture; `RequestsOpenAccess` does not change that. So the mic key
/// asks the container app to record, and this class waits for text to appear in the
/// shared container and inserts it at the cursor.
///
/// Two ways to ask, and the choice is made per tap:
///
/// - The app is alive (its heartbeat is recent): post a Darwin notification. Nothing
///   visibly switches, which is the whole point.
/// - It is not: open `whimprflow://dictate`. iOS shows a back arrow to return here.
///   Slower, and always works.
///
/// Preferring the first and falling back to the second is deliberate. A stale "the
/// app is alive" would leave the mic key silently doing nothing, which is the one
/// failure mode worth engineering against — hence a heartbeat rather than a flag.
final class KeyboardViewController: UIInputViewController {
    private var micButton: UIButton!
    private var hintLabel: UILabel!
    private var lastInsertedResultID = 0
    private var isDictating = false

    // MARK: - Lifecycle

    override func viewDidLoad() {
        super.viewDidLoad()
        buildInterface()
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
        refresh()
    }

    // MARK: - Interface

    private func buildInterface() {
        view.backgroundColor = UIColor(red: 0x11 / 255, green: 0x14 / 255, blue: 0x19 / 255, alpha: 1)

        micButton = UIButton(type: .system)
        micButton.setImage(UIImage(systemName: "mic.fill"), for: .normal)
        micButton.tintColor = UIColor(red: 0xDA / 255, green: 0xF3 / 255, blue: 0xEA / 255, alpha: 1)
        micButton.backgroundColor = UIColor(red: 0x1C / 255, green: 0x21 / 255, blue: 0x2A / 255, alpha: 1)
        micButton.layer.cornerRadius = 32
        micButton.layer.cornerCurve = .continuous
        micButton.accessibilityLabel = "Dictate"
        micButton.addTarget(self, action: #selector(micTapped), for: .touchUpInside)

        hintLabel = UILabel()
        hintLabel.font = .preferredFont(forTextStyle: .caption1)
        hintLabel.textColor = UIColor(red: 0x8A / 255, green: 0x93 / 255, blue: 0xA3 / 255, alpha: 1)
        hintLabel.textAlignment = .center
        hintLabel.numberOfLines = 2

        let globe = auxiliaryButton(symbol: "globe", action: #selector(switchKeyboard))
        // `advanceToNextInputMode` is the only sanctioned way off a custom keyboard,
        // and without a control for it the user can be stranded here.
        let delete = auxiliaryButton(symbol: "delete.left", action: #selector(deleteTapped))
        let space = auxiliaryButton(title: "space", action: #selector(spaceTapped))
        let ret = auxiliaryButton(symbol: "return", action: #selector(returnTapped))

        let row = UIStackView(arrangedSubviews: [globe, delete, space, ret])
        row.axis = .horizontal
        row.distribution = .fillEqually
        row.spacing = 8

        let stack = UIStackView(arrangedSubviews: [micButton, hintLabel, row])
        stack.axis = .vertical
        stack.alignment = .fill
        stack.spacing = 10
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)

        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 12),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -12),
            stack.topAnchor.constraint(equalTo: view.topAnchor, constant: 10),
            stack.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -10),
            micButton.heightAnchor.constraint(equalToConstant: 64),
            row.heightAnchor.constraint(equalToConstant: 42),
            // A keyboard has no intrinsic height; without this it collapses.
            view.heightAnchor.constraint(greaterThanOrEqualToConstant: 190),
        ])
    }

    private func auxiliaryButton(symbol: String? = nil, title: String? = nil, action: Selector) -> UIButton {
        let button = UIButton(type: .system)
        if let symbol { button.setImage(UIImage(systemName: symbol), for: .normal) }
        if let title { button.setTitle(title, for: .normal) }
        button.tintColor = UIColor(red: 0xB8 / 255, green: 0xC0 / 255, blue: 0xCC / 255, alpha: 1)
        button.setTitleColor(button.tintColor, for: .normal)
        button.titleLabel?.font = .preferredFont(forTextStyle: .body)
        button.backgroundColor = UIColor(red: 0x28 / 255, green: 0x30 / 255, blue: 0x3B / 255, alpha: 1)
        button.layer.cornerRadius = 8
        button.layer.cornerCurve = .continuous
        button.addTarget(self, action: action, for: .touchUpInside)
        return button
    }

    // MARK: - Actions

    @objc private func micTapped() {
        // Without Full Access there is no container and no network; say so rather
        // than appearing to work.
        guard Handoff.isReachable else {
            hintLabel.text = "Turn on Allow Full Access in Settings to dictate."
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
        hintLabel.text = "Could not open WhimprFlow."
    }

    @objc private func switchKeyboard() { advanceToNextInputMode() }
    @objc private func deleteTapped() { textDocumentProxy.deleteBackward() }
    @objc private func spaceTapped() { textDocumentProxy.insertText(" ") }
    @objc private func returnTapped() { textDocumentProxy.insertText("\n") }

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

    /// Insert anything new, and reflect the app's state in the button.
    private func refresh() {
        let state = Handoff.state
        isDictating = (state == .recording)

        if let result = Handoff.latestResult(), result.id > lastInsertedResultID {
            lastInsertedResultID = result.id
            textDocumentProxy.insertText(result.text)
        }

        switch state {
        case .recording:
            micButton.setImage(UIImage(systemName: "stop.fill"), for: .normal)
            hintLabel.text = "Listening — tap to finish"
        case .transcribing:
            micButton.setImage(UIImage(systemName: "waveform"), for: .normal)
            hintLabel.text = "Transcribing"
        case .failed:
            micButton.setImage(UIImage(systemName: "mic.fill"), for: .normal)
            hintLabel.text = "Dictation failed — open WhimprFlow for details"
        case .idle:
            micButton.setImage(UIImage(systemName: "mic.fill"), for: .normal)
            hintLabel.text = Handoff.isReachable
                ? nil
                : "Turn on Allow Full Access in Settings to dictate."
        }
    }
}
