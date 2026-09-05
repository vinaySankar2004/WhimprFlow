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

    /// Re-read the two things the observation graph cannot see for itself.
    func refreshConfiguration() {
        hasAPIKey = APIKey.isSet
        hasMicPermission = Recorder.hasPermission
    }

    // MARK: - Lifecycle

    /// Begin publishing the liveness heartbeat the keyboard reads.
    ///
    /// A heartbeat rather than a flag: the app can be killed without getting to clear
    /// a flag, and a stale "I'm alive" would leave the mic key silently doing
    /// nothing. Stopping it is therefore also how the keyboard learns to bounce.
    func startHeartbeat() {
        guard settings.keepSessionAlive, heartbeat == nil else { return }
        Handoff.markAlive()
        heartbeat = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(Handoff.livenessWindow / 3))
                guard self != nil else { return }
                Handoff.markAlive()
            }
        }
    }

    func stopHeartbeat() {
        heartbeat?.cancel()
        heartbeat = nil
        Handoff.clearAlive()
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
            try recorder.start()
            phase = .recording
            Handoff.state = .recording
        } catch {
            fail(error.localizedDescription)
        }
    }

    func cancelRecording() {
        guard phase == .recording else { return }
        _ = recorder.stop()
        phase = .idle
        Handoff.state = .idle
    }

    func finishRecording() async {
        guard phase == .recording else { return }
        let samples = recorder.stop()
        phase = .transcribing
        Handoff.state = .transcribing

        // A tap rather than a dictation. Nothing to send, and an empty request would
        // come back as a hallucinated sentence rather than an empty string.
        guard samples.count > Int(Recorder.sampleRate) / 4 else {
            phase = .idle
            Handoff.state = .idle
            return
        }

        guard let key = APIKey.load() else {
            fail("no API key is set")
            return
        }
        let client = GroqClient(apiKey: key)
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
