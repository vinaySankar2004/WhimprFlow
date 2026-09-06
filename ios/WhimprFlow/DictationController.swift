import Foundation
import Observation
import UIKit

/// The dictation loop: record, recognize, clean, publish.
///
/// This is the iOS counterpart of `hotkey.rs`, and like it, it decides nothing about
/// the text. Recognition and cleanup are network calls; everything between and around
/// them is `whimpr-core` through the bridge. The order in particular is not
/// reimplemented here — `prepare` and `finish` own it.
@MainActor
@Observable
final class DictationController {
    static let shared = DictationController()

    enum Phase: Equatable {
        case idle
        case recording
        case transcribing
        /// Terminal states linger briefly so they can be seen; the view drives that,
        /// not this type — the same reason the Mac's shell lingers rather than
        /// emitting a terminal bar state followed immediately by `Idle`.
        case done(String)
        case failed(String)
    }

    private(set) var phase: Phase = .idle
    private(set) var lastEngine: Engine?
    private(set) var lastDegraded: String?

    let recorder = Recorder()
    private var settings: Settings { .shared }
    private var heartbeat: Task<Void, Never>?

    var level: Float { recorder.level }
    var isRecording: Bool { phase == .recording }

    /// Whether a dictation can start at all, and why not when it cannot.
    ///
    /// Backed by stored properties rather than reading the Keychain and the
    /// permission store on each access: those live outside the observation graph, so
    /// a computed version leaves the view showing "add your API key" after the key
    /// has been saved, with no way for SwiftUI to know it should redraw. Call
    /// `refreshConfiguration()` whenever either could have changed.
    var blocker: String? {
        if !hasAPIKey { return "Add your Groq API key in Settings to start dictating." }
        if !hasMicPermission { return "WhimprFlow needs microphone access." }
        return nil
    }

    private(set) var hasAPIKey = APIKey.isSet
    private(set) var hasMicPermission = Recorder.hasPermission

    /// The keys with their rate-limit clocks, kept across dictations so a limited
    /// key stays skipped for as long as Groq asked. Rebuilt when the list changes.
    private var ring: KeyRing? = try? KeyRing(keys: APIKey.loadAll())

    /// Re-read the things the observation graph cannot see for itself.
    func refreshConfiguration() {
        hasAPIKey = APIKey.isSet
        hasMicPermission = Recorder.hasPermission
        ring = try? KeyRing(keys: APIKey.loadAll())
    }

    // MARK: - Lifecycle

    /// Enter standby: hold the microphone open and publish the liveness heartbeat, so
    /// the keyboard's mic key can start a dictation without opening this app.
    ///
    /// The engine has to be genuinely *running*, not merely permitted to run.
    /// `UIBackgroundModes: audio` keeps an app alive only while audio is active;
    /// declaring the mode and idling gets the app suspended seconds after it
    /// backgrounds, which silently made every mic-key tap fall back to opening it.
    ///
    /// The heartbeat is a heartbeat and not a flag because the app can be killed
    /// without getting to clear a flag, and a stale "I'm alive" leaves the mic key
    /// doing nothing at all. Its stopping is how the keyboard learns to bounce.
    func startStandby() {
        guard settings.keepSessionAlive else { return }
        guard blocker == nil else { return }
        // A call or Siri arriving mid-sentence takes the microphone away. Transcribe
        // what was captured rather than discarding it: the words up to the
        // interruption are still the user's, and losing them silently is worse than
        // an ending that stops short of where they meant to finish.
        recorder.onCaptureInterrupted = { [weak self] in
            Task { @MainActor in await self?.finishRecording() }
        }
        do {
            try recorder.startEngine()
        } catch {
            // Standby is an optimization; failing it must not break dictation, which
            // still works by opening the app.
            stopStandby()
            return
        }
        guard heartbeat == nil else { return }
        Handoff.markAlive()
        heartbeat = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(Handoff.livenessWindow / 3))
                guard let self, self.recorder.isEngineRunning else { return }
                Handoff.markAlive()
            }
        }
    }

    /// Leave standby: release the microphone and stop claiming to be reachable.
    func stopStandby() {
        heartbeat?.cancel()
        heartbeat = nil
        Handoff.clearAlive()
        // Never while a dictation is in flight — that would drop the recording.
        if !recorder.isCapturing {
            recorder.stopEngine()
        }
    }

    /// Re-read the standby preference and act on it.
    func applyStandbyPreference() {
        if settings.keepSessionAlive {
            startStandby()
        } else {
            stopStandby()
        }
    }

    /// Listen for the keyboard's mic key.
    ///
    /// Darwin notifications are the only cross-process wake-up an extension can send,
    /// and they carry no payload — so these mean exactly "start" and "stop", and
    /// everything else is read from the shared container.
    func observeKeyboard() {
        let observer = Unmanaged.passUnretained(self).toOpaque()
        Handoff.observe(.start, observer: observer) { _, observer, _, _, _ in
            guard let observer else { return }
            let controller = Unmanaged<DictationController>
                .fromOpaque(observer).takeUnretainedValue()
            Task { @MainActor in controller.startRecording() }
        }
        Handoff.observe(.stop, observer: observer) { _, observer, _, _, _ in
            guard let observer else { return }
            let controller = Unmanaged<DictationController>
                .fromOpaque(observer).takeUnretainedValue()
            Task { @MainActor in await controller.finishRecording() }
        }
        Handoff.observe(.cancel, observer: observer) { _, observer, _, _, _ in
            guard let observer else { return }
            let controller = Unmanaged<DictationController>
                .fromOpaque(observer).takeUnretainedValue()
            Task { @MainActor in controller.cancelRecording() }
        }
    }

    // MARK: - The loop

    func toggle() {
        switch phase {
        case .recording: Task { await finishRecording() }
        default: startRecording()
        }
    }

    func startRecording() {
        guard blocker == nil else {
            phase = .failed(blocker ?? "not configured")
            return
        }
        guard phase != .recording, phase != .transcribing else { return }
        do {
            try recorder.beginCapture()
            phase = .recording
            Handoff.state = .recording
        } catch {
            fail(error.localizedDescription)
        }
    }

    func cancelRecording() {
        guard phase == .recording else { return }
        _ = recorder.endCapture()
        phase = .idle
        Handoff.state = .idle
    }

    func finishRecording() async {
        guard phase == .recording else { return }
        let samples = recorder.endCapture()
        phase = .transcribing
        Handoff.state = .transcribing

        // A tap rather than a dictation. Nothing to send, and an empty request would
        // come back as a hallucinated sentence rather than an empty string.
        guard samples.count > Int(Recorder.sampleRate) / 4 else {
            phase = .idle
            Handoff.state = .idle
            return
        }

        guard let ring, ring.count > 0 else {
            fail("no API key is set")
            return
        }
        let client = GroqClient(ring: ring)
        let dictionary = settings.dictionary
        let level = settings.level

        do {
            let transcript = try await recognize(samples: samples, client: client, dictionary: dictionary)
            guard !transcript.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                phase = .idle
                Handoff.state = .idle
                return
            }
            let finished = try await clean(
                transcript: transcript, client: client, dictionary: dictionary, level: level
            )
            publish(finished)
        } catch {
            fail(error.localizedDescription)
        }
    }

    // MARK: - Recognition

    /// Two passes, and only when the dictionary matched.
    ///
    /// Prompting biases every dictation, including the overwhelming majority with no
    /// dictionary word in them — and a prompt Whisper cannot hear in the audio is one
    /// it may emit anyway. Running unprompted first means the common case is
    /// untouched and the biased result has an unbiased one to be checked against.
    /// Without that comparison a hallucinated name goes straight to the cursor.
    private func recognize(
        samples: [Float], client: GroqClient, dictionary: DictionaryStore
    ) async throws -> String {
        let wav = Recorder.wav(from: samples)
        let unprompted = try await client.transcribe(wav: wav, prompt: nil)

        let bias = try WhimprCore.asrBiasPrompt(unprompted: unprompted, dictionary: dictionary)
        // No match means one pass was the whole job. Do not prompt anyway.
        guard let prompt = bias.prompt else { return unprompted }

        guard let prompted = try? await client.transcribe(wav: wav, prompt: prompt) else {
            return unprompted // a failed second pass is not a failed dictation
        }
        let accept = try WhimprCore.asrAcceptPrompted(
            unprompted: unprompted, prompted: prompted, vocab: bias.vocab
        )
        return accept ? prompted : unprompted
    }

    // MARK: - Cleanup

    /// Cleanup, with raw as the fallback rather than as a failure.
    ///
    /// Every path here ends in text worth inserting: a cloud error, a truncated
    /// reply, or a gate rejection all produce the raw transcript with the reason
    /// recorded. An untidy-but-faithful paste beats a wrong-but-clean one, and beats
    /// nothing at all by a wider margin still.
    private func clean(
        transcript: String, client: GroqClient, dictionary: DictionaryStore, level: CleanupLevel
    ) async throws -> Finished {
        let prepared = try WhimprCore.prepare(
            raw: transcript, level: level, dictionary: dictionary
        )
        guard level != .none else {
            return try WhimprCore.rawOnly(
                prepared: prepared, degraded: nil, dictionary: dictionary, rawMode: true
            )
        }
        do {
            let reply = try await client.cleanup(prepared: prepared)
            return try WhimprCore.finish(
                prepared: prepared, modelOutput: reply, engine: .cloud, dictionary: dictionary
            )
        } catch {
            // No local model to fall back to on this platform, so raw is the floor.
            return try WhimprCore.rawOnly(
                prepared: prepared,
                degraded: "cloud_error: \(error.localizedDescription)",
                dictionary: dictionary
            )
        }
    }

    // MARK: - Output

    private func publish(_ finished: Finished) {
        lastEngine = finished.engine
        lastDegraded = finished.degraded
        Handoff.publish(finished)
        // Also on the pasteboard, so a dictation is usable even when the keyboard is
        // not installed or Full Access is off.
        UIPasteboard.general.string = finished.text
        phase = .done(finished.text)
        Handoff.state = .idle
    }

    private func fail(_ message: String) {
        phase = .failed(message)
        Handoff.state = .failed
    }

    func clearTerminalPhase() {
        switch phase {
        case .done, .failed: phase = .idle
        default: break
        }
    }
}
