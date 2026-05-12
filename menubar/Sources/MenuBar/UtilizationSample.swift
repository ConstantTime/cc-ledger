import Foundation

struct UtilizationSample: Codable, Sendable, Hashable {
    let timestamp: Date
    let utilization: Double      // 0..100
}

/// Persists 5-hour utilization snapshots across app launches via UserDefaults.
/// Pruned to the current session by the caller so the array stays small
/// (~300 samples max at 60s cadence over a 5-hour window).
enum UtilizationSampleStore {
    private static let key = "menubar.fiveHourSamples.v1"

    static func load() -> [UtilizationSample] {
        guard let data = UserDefaults.standard.data(forKey: key) else { return [] }
        return (try? JSONDecoder().decode([UtilizationSample].self, from: data)) ?? []
    }

    static func save(_ samples: [UtilizationSample]) {
        if let data = try? JSONEncoder().encode(samples) {
            UserDefaults.standard.set(data, forKey: key)
        }
    }
}
