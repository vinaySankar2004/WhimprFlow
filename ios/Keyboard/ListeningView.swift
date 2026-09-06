import UIKit

/// What the key area shows while a dictation is happening, and afterwards when it
/// went wrong.
///
/// Listening is the waveform, the word, and which microphone — the same three things
/// Wispr Flow's screen shows — plus how long you have been talking, which theirs
/// does not. Transcribing and failure are states of their own rather than a label
/// swapped on the mic key: a failure with no button to retry it is a failure you
/// have to leave the keyboard to recover from.
final class ListeningView: UIView {
    enum Mode: Equatable {
        case listening
        case transcribing
        /// Something to say and a way back: retry, or open the app.
        case failed(String, canRetry: Bool)
    }

    var onRetry: (() -> Void)?
    var onOpenApp: (() -> Void)?

    let waveform = WaveformView()
    private let spinner = UIActivityIndicatorView(style: .large)
    private let title = UILabel()
    private let subtitle = UILabel()
    private let retryButton = UIButton(type: .system)
    private let openButton = UIButton(type: .system)
    private let buttons = UIStackView()

    private var timer: Timer?
    private(set) var mode: Mode = .listening

    override init(frame: CGRect) {
        super.init(frame: frame)
        build()
    }

    required init?(coder: NSCoder) { fatalError("not used") }

    private func build() {
        title.font = .systemFont(ofSize: 20, weight: .medium)
        title.textColor = Palette.textPrimary
        title.textAlignment = .center

        subtitle.font = .systemFont(ofSize: 15, weight: .regular)
        subtitle.textColor = Palette.textSecondary
        subtitle.textAlignment = .center
        subtitle.numberOfLines = 3

        spinner.color = Palette.textSecondary
        spinner.hidesWhenStopped = true

        retryButton.configuration = Self.buttonConfiguration(title: "Try again", symbol: "arrow.clockwise", prominent: true)
        retryButton.addTarget(self, action: #selector(retryTapped), for: .touchUpInside)
        openButton.configuration = Self.buttonConfiguration(title: "Open WhimprFlow", symbol: "arrow.up.forward.app", prominent: false)
        openButton.addTarget(self, action: #selector(openTapped), for: .touchUpInside)

        buttons.axis = .horizontal
        buttons.spacing = 10
        buttons.addArrangedSubview(retryButton)
        buttons.addArrangedSubview(openButton)

        let stack = UIStackView(arrangedSubviews: [waveform, spinner, title, subtitle, buttons])
        stack.axis = .vertical
        stack.alignment = .center
        stack.spacing = 6
        stack.setCustomSpacing(24, after: waveform)
        stack.setCustomSpacing(18, after: spinner)
        stack.setCustomSpacing(16, after: subtitle)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)

        NSLayoutConstraint.activate([
            stack.centerXAnchor.constraint(equalTo: centerXAnchor),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor, constant: -6),
            stack.leadingAnchor.constraint(greaterThanOrEqualTo: leadingAnchor, constant: 24),
            stack.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -24),
            waveform.heightAnchor.constraint(equalToConstant: 44),
            waveform.widthAnchor.constraint(equalToConstant: waveform.intrinsicContentSize.width),
        ])
        apply(.listening)
    }

    private static func buttonConfiguration(title: String, symbol: String, prominent: Bool) -> UIButton.Configuration {
        var configuration = prominent ? UIButton.Configuration.filled() : UIButton.Configuration.gray()
        configuration.title = title
        configuration.image = UIImage(systemName: symbol, withConfiguration: UIImage.SymbolConfiguration(pointSize: 13, weight: .semibold))
        configuration.imagePadding = 5
        configuration.cornerStyle = .capsule
        configuration.buttonSize = .small
        configuration.attributedTitle = AttributedString(
            title, attributes: AttributeContainer([.font: UIFont.systemFont(ofSize: 14, weight: .semibold)])
        )
        if prominent {
            configuration.baseBackgroundColor = Palette.pill
            configuration.baseForegroundColor = Palette.pillText
        } else {
            configuration.baseBackgroundColor = Palette.barControl
            configuration.baseForegroundColor = Palette.textPrimary
        }
        return configuration
    }

    // MARK: - State

    func setMode(_ mode: Mode) {
        guard mode != self.mode else { return }
        self.mode = mode
        UIView.transition(with: self, duration: 0.18, options: [.transitionCrossDissolve, .allowUserInteraction]) {
            self.apply(mode)
        }
    }

    private func apply(_ mode: Mode) {
        timer?.invalidate()
        timer = nil
        switch mode {
        case .listening:
            waveform.isHidden = false
            waveform.start()
            spinner.stopAnimating()
            buttons.isHidden = true
            title.font = .systemFont(ofSize: 20, weight: .medium)
            title.text = "Listening"
            tickElapsed()
            // Once a second is enough for a counter that reads m:ss; the waveform
            // carries the sense of liveness.
            timer = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
                self?.tickElapsed()
            }
        case .transcribing:
            waveform.stop()
            waveform.isHidden = true
            spinner.startAnimating()
            buttons.isHidden = true
            title.font = .systemFont(ofSize: 20, weight: .medium)
            title.text = "Transcribing…"
            subtitle.text = "Cleaning up what you said"
        case let .failed(reason, canRetry):
            waveform.stop()
            waveform.isHidden = true
            spinner.stopAnimating()
            title.text = "Dictation didn't go through"
            title.font = .systemFont(ofSize: 17, weight: .semibold)
            subtitle.text = reason
            retryButton.isHidden = !canRetry
            buttons.isHidden = false
        }
    }

    /// Stop the display link and the clock. Called when the view leaves the screen.
    func pause() {
        waveform.stop()
        timer?.invalidate()
        timer = nil
    }

    private func tickElapsed() {
        let input = Handoff.inputName ?? "Microphone"
        guard let startedAt = Handoff.captureStartedAt else {
            subtitle.text = input
            return
        }
        let seconds = max(0, Int(Date().timeIntervalSince(startedAt)))
        subtitle.text = String(format: "%@ · %d:%02d", input, seconds / 60, seconds % 60)
    }

    @objc private func retryTapped() { onRetry?() }
    @objc private func openTapped() { onOpenApp?() }
}
