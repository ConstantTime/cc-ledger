import Foundation

/// Per-file aggregates we cache so we don't re-parse JSONL on every refresh tick.
/// Bumped when file mtime/size changes (append-only logs make this safe).
struct ParsedFile: Codable, Sendable {
    let path: String
    let mtime: Date
    let size: Int64

    // dayKey → model → tokens
    var dayBuckets: [String: [String: ModelTokens]]

    // hourKey → bucket (covers all hours present in the file; queries filter by recency)
    var hourBuckets: [String: HourBucket]

    // dayKey → list of thinking-block lengths (chars or signature length)
    var thinkingLengthsByDay: [String: [Int]]

    // dayKey → tool name → count
    var toolByDay: [String: [String: Int]]

    // Per-session metadata (one ParsedFile == one session JSONL).
    var sessionId: String?              // UUID from <sessionId>.jsonl filename
    var projectPath: String?            // decoded "/Users/.../code/menubar"
    var cwd: String?                    // authoritative cwd captured from JSONL turns
    var firstActivityAt: Date?
    var lastActivityAt: Date?
    var turnCount: Int                  // # of post-dedup assistant rows
    var totalCostUSD: Double            // sum of per-row pricing
    var isSubagent: Bool                // true if path contains "/subagents/"
}

struct ModelTokens: Codable, Sendable, Hashable {
    var input: Int
    var output: Int
    var cacheRead: Int
    var cacheCreation: Int

    init(input: Int = 0, output: Int = 0, cacheRead: Int = 0, cacheCreation: Int = 0) {
        self.input = input
        self.output = output
        self.cacheRead = cacheRead
        self.cacheCreation = cacheCreation
    }
}

struct HourBucket: Codable, Sendable {
    let hourStart: Date
    var perModel: [String: ModelTokens]
}

struct ScanCache: Codable, Sendable {
    /// Bump when ParsedFile shape changes — older caches will be rejected and rebuilt.
    /// v2: dedupe streaming chunks by (messageId, requestId) and skip sidechain rows
    ///     before aggregation, matching CodexBar's scanner.
    /// v3: keep sidechain rows (CodexBar does too — only cross-file dedup excludes
    ///     them, and that's a rare overlap our v2 was over-correcting).
    /// v4: ParsedFile gains per-session summary fields (sessionId, projectPath,
    ///     first/lastActivityAt, turnCount, totalCostUSD, isSubagent) for the
    ///     active-sessions list.
    /// v5: ParsedFile gains `cwd` captured from JSONL turns — authoritative
    ///     project path, especially for worktrees whose slug round-trips wrong.
    static let currentVersion: Int = 5
    var version: Int
    var files: [String: ParsedFile]   // keyed by absolute path

    init(files: [String: ParsedFile] = [:]) {
        self.version = ScanCache.currentVersion
        self.files = files
    }
}

enum ScanCacheStore {
    static func cacheURL() -> URL {
        let appSupport = FileManager.default.urls(
            for: .applicationSupportDirectory, in: .userDomainMask
        ).first!
        return appSupport
            .appendingPathComponent("MenuBar", isDirectory: true)
            .appendingPathComponent("scan-cache.json")
    }

    static func load() -> ScanCache {
        guard let data = try? Data(contentsOf: cacheURL()),
              let cache = try? JSONDecoder().decode(ScanCache.self, from: data),
              cache.version == ScanCache.currentVersion
        else { return ScanCache() }
        return cache
    }

    static func save(_ cache: ScanCache) {
        let url = cacheURL()
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        if let data = try? JSONEncoder().encode(cache) {
            try? data.write(to: url, options: .atomic)
        }
    }
}
