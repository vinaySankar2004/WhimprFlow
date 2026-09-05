import UIKit

/// The bars that react to the voice while dictating.
///
/// Reads the level out of `LevelChannel` on a display link rather than being pushed
/// values: the writer is an audio callback in another process, and a pull at screen
/// refresh is both simpler and exactly as fresh as the display can show.
final class WaveformView: UIView {
    private let barCount = 9
    private var bars: [UIView] = []
    private var displayLink: CADisplayLink?

    /// Per-bar smoothed heights, so neighbouring bars lag each other slightly and the
    /// row reads as a travelling wave rather than nine copies of one number.
    private var smoothed: [CGFloat]

    /// How hard the level is driven before clipping.
    ///
    /// The level arriving is already log-scaled for display, and ordinary speech sits
    /// low in that range — around 0.35–0.6. Drawn one-to-one the bars barely leave the
    /// floor and the keyboard looks like it is not listening. This maps the part of
    /// the range speech actually occupies onto the full height.
    private let floor: CGFloat = 0.18
    private let ceiling: CGFloat = 0.75

    override init(frame: CGRect) {
        smoothed = Array(repeating: 0, count: barCount)
        super.init(frame: frame)
        build()
    }

    required init?(coder: NSCoder) {
        smoothed = Array(repeating: 0, count: barCount)
        super.init(coder: coder)
        build()
    }

    private func build() {
        for _ in 0..<barCount {
            let bar = UIView()
            bar.backgroundColor = Palette.waveBar
            bar.layer.cornerRadius = 2.5
            bar.layer.cornerCurve = .continuous
            addSubview(bar)
            bars.append(bar)
        }
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        layoutBars()
    }

    private func layoutBars() {
        let width: CGFloat = 5
        let spacing: CGFloat = 6
        let total = CGFloat(barCount) * width + CGFloat(barCount - 1) * spacing
        var x = (bounds.width - total) / 2

        for (index, bar) in bars.enumerated() {
            let height = max(5, smoothed[index] * bounds.height)
            bar.frame = CGRect(
                x: x,
                y: (bounds.height - height) / 2,
                width: width,
                height: height
            )
            x += width + spacing
        }
    }

    // MARK: - Driving

    func start() {
        guard displayLink == nil else { return }
        let link = CADisplayLink(target: self, selector: #selector(tick))
        // 30 is plenty for nine bars and halves the wake-ups of a 60 Hz link inside a
        // keyboard extension, which has a much tighter memory and CPU budget than an
        // app does.
        link.preferredFramesPerSecond = 30
        link.add(to: .main, forMode: .common)
        displayLink = link
    }

    func stop() {
        displayLink?.invalidate()
        displayLink = nil
        smoothed = Array(repeating: 0, count: barCount)
        // Deliberately not animated. `stop()` is called from `viewWillDisappear`,
        // which is *during* the keyboard's dismissal transition, and starting a
        // 0.2-second animation there animates against the system's own — the bars
        // visibly slide while the keyboard is already sliding away. Settling
        // instantly is invisible on a view that is leaving the screen anyway.
        layoutBars()
    }

    @objc private func tick() {
        let raw = CGFloat(LevelChannel.shared.level)
        // Expand the band speech occupies to the full 0…1 the bars draw.
        let scaled = min(max((raw - floor) / (ceiling - floor), 0), 1)

        // Shift the history along by one and seed the middle, so a syllable travels
        // outward from the centre instead of every bar jumping together.
        let centre = barCount / 2
        for offset in stride(from: centre, to: 0, by: -1) {
            smoothed[centre - offset] = smoothed[centre - offset + 1]
            smoothed[centre + offset] = smoothed[centre + offset - 1]
        }
        // Asymmetric smoothing: rise quickly so an onset is not missed, fall slowly so
        // the row does not flicker between syllables.
        let target = scaled
        let previous = smoothed[centre]
        smoothed[centre] = target > previous
            ? previous + (target - previous) * 0.6
            : previous + (target - previous) * 0.25

        layoutBars()
    }
}
