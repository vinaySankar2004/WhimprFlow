import ActivityKit
import Foundation
import UIKit

/// Owns the one Live Activity that stands for the standby session.
///
/// # Rules that are iOS's, not ours
///
/// - An activity can be *requested* only while the app is in the foreground; from the
///   background the request fails with "Target is not foreground". It can be updated
///   and ended from anywhere. So `ensure` requests when it can and otherwise leaves a
///   note for the next foreground, and standby itself never waits on it.
/// - The system ends every activity after eight hours. With the default five-minute
///   timeout that never shows; with "Always" the island glyph disappears after eight
///   hours and comes back on the next app visit. Standby is unaffected either way.
@MainActor
final class StandbyActivityController {
    private var activity: Activity<StandbyActivityAttributes>?
    private var wanted: StandbyActivityAttributes.ContentState?

    init() {
        // After a relaunch the previous process's activity may still be on screen.
        // Adopt one and end the rest, or each launch adds a glyph to the island.
        let existing = Activity<StandbyActivityAttributes>.activities
        activity = existing.first
        for stale in existing.dropFirst() {
            Task { await stale.end(nil, dismissalPolicy: .immediate) }
        }
    }

    /// Show `state`, starting the activity if there is none and iOS allows it now.
    func ensure(_ state: StandbyActivityAttributes.ContentState) {
        wanted = state
        if let activity, activity.activityState == .active {
            Task { await activity.update(ActivityContent(state: state, staleDate: nil)) }
            return
        }
        activity = nil
        guard UIApplication.shared.applicationState == .active else { return }
        guard ActivityAuthorizationInfo().areActivitiesEnabled else { return }
        do {
            activity = try Activity.request(
                attributes: StandbyActivityAttributes(),
                content: ActivityContent(state: state, staleDate: nil)
            )
        } catch {
            // Not fatal: the mic is still ready, only the island is quiet. The
            // next foreground tries again.
            activity = nil
        }
    }

    /// Called on every foreground so an activity that could not be requested from
    /// the background — or that iOS ended at its eight-hour limit — is put back.
    func resumeIfNeeded() {
        guard let wanted else { return }
        ensure(wanted)
    }

    func end() {
        wanted = nil
        guard let activity else { return }
        self.activity = nil
        Task { await activity.end(nil, dismissalPolicy: .immediate) }
    }
}
