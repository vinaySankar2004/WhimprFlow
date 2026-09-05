import Foundation

/// The live microphone level, shared from the app to the keyboard.
///
/// # Why shared memory and not the other two channels
///
/// `Handoff` already has both an event channel and a data channel, and neither suits
/// a value that changes thirty times a second:
///
/// - **Darwin notifications** carry no payload, so each one would only mean "read the
///   level", and posting one per audio frame floods a system-wide notification centre
///   for a waveform.
/// - **App Group `UserDefaults`** goes through `cfprefsd` on every write. At this rate
///   that is a synchronisation cost per frame, paid by both processes, to move four
///   bytes.
///
/// A single `Float` in a memory-mapped file is the right size of mechanism: the write
/// is a store to memory, the read is a load, and the kernel handles coherency between
/// the two processes.
///
/// # Consistency
///
/// A naturally aligned 32-bit store is atomic on arm64, so a reader can never observe
/// a half-written value — only a slightly stale one, which is invisible in a waveform.
/// No locking, and nothing for the audio thread to block on.
final class LevelChannel {
    /// Nil when the App Group is unreachable — in the keyboard, that means Allow Full
    /// Access is off, and the waveform simply stays flat.
    private let pointer: UnsafeMutablePointer<Float>?
    private let descriptor: Int32
    private let mapped: UnsafeMutableRawPointer?

    static let shared = LevelChannel()

    private init() {
        guard let container = FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: Handoff.appGroup)
        else {
            pointer = nil
            descriptor = -1
            mapped = nil
            return
        }
        let path = container.appendingPathComponent("level.bin").path
        let size = MemoryLayout<Float>.size

        let fd = open(path, O_RDWR | O_CREAT, 0o644)
        guard fd >= 0 else {
            pointer = nil
            descriptor = -1
            mapped = nil
            return
        }
        // Must exist at full size before mapping: mapping past the end of a file and
        // then touching those pages raises SIGBUS rather than returning an error.
        if ftruncate(fd, off_t(size)) != 0 {
            close(fd)
            pointer = nil
            descriptor = -1
            mapped = nil
            return
        }
        let address = mmap(nil, size, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0)
        guard let address, address != MAP_FAILED else {
            close(fd)
            pointer = nil
            descriptor = -1
            mapped = nil
            return
        }
        descriptor = fd
        mapped = address
        pointer = address.bindMemory(to: Float.self, capacity: 1)
    }

    deinit {
        if let mapped { munmap(mapped, MemoryLayout<Float>.size) }
        if descriptor >= 0 { close(descriptor) }
    }

    /// 0…1, already log-scaled for display. Reads 0 when the channel is unavailable.
    var level: Float {
        get { pointer?.pointee ?? 0 }
        set { pointer?.pointee = min(max(newValue, 0), 1) }
    }

    /// Whether the channel actually opened.
    var isAvailable: Bool { pointer != nil }
}
