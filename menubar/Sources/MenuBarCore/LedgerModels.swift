import Foundation

/// Status of `~/.cc-ledger/ledger.db` from menubar's perspective.
public enum LedgerStatus: Sendable, Equatable {
    case unknown                                  // not yet checked
    case absent                                   // file doesn't exist
    case versionMismatch(actual: Int, expected: Int)
    case readError(String)
    case available(version: Int)
}

/// One row from `agent_turns` grouped by category.
public struct LedgerActivityCategory: Sendable, Identifiable, Hashable {
    public let category: String
    public let turnCount: Int
    public let costUSD: Double
    public let totalTokens: Int

    public var id: String { category }

    public init(category: String, turnCount: Int, costUSD: Double, totalTokens: Int) {
        self.category = category
        self.turnCount = turnCount
        self.costUSD = costUSD
        self.totalTokens = totalTokens
    }

    /// Friendly display label — title-cased, slashes spaced.
    public var displayName: String {
        switch category {
        case "build/deploy": return "Build / Deploy"
        case "git":          return "Git"
        default:
            let parts = category.split(separator: "/").map { part -> String in
                let s = String(part)
                guard let first = s.first else { return s }
                return first.uppercased() + s.dropFirst()
            }
            return parts.joined(separator: " / ")
        }
    }
}

/// Cost-distribution stats across every non-zero PR rollup in the DB —
/// the "what does a normal PR cost vs. how bad does it get" snapshot.
/// Nil when fewer than 5 PRs are on file (percentiles become noise).
public struct LedgerPRCostStats: Sendable, Equatable {
    public let count: Int
    public let p50: Double
    public let p95: Double
    public let p99: Double
    public let maxCost: Double
    /// PRs with `totalCostUSD > runawayThreshold` get the runaway badge.
    public let runawayThreshold: Double

    public init(count: Int, p50: Double, p95: Double, p99: Double, maxCost: Double, runawayThreshold: Double) {
        self.count = count
        self.p50 = p50
        self.p95 = p95
        self.p99 = p99
        self.maxCost = maxCost
        self.runawayThreshold = runawayThreshold
    }
}

/// Cost-distribution stats across every non-zero session in the DB —
/// the "what does a normal session cost vs. how bad does it get" snapshot,
/// the session counterpart to `LedgerPRCostStats`. Nil when fewer than 5
/// sessions are on file (percentiles become noise).
public struct LedgerSessionCostStats: Sendable, Equatable {
    public let count: Int
    public let p50: Double
    public let p95: Double
    public let p99: Double
    public let maxCost: Double
    /// Sessions with total cost > `runawayThreshold` get the runaway badge.
    /// Matches the PR-stats convention (`p95`).
    public let runawayThreshold: Double
    /// Number of sessions with cost > `runawayThreshold` across the full DB
    /// (not just a recent window) — runaway means "unusual for our history."
    /// Computed in `LedgerStore.sessionCostStats` since there is no separate
    /// recent-sessions list to filter against (unlike the PR panel).
    public let runawayCount: Int

    public init(count: Int, p50: Double, p95: Double, p99: Double, maxCost: Double, runawayThreshold: Double, runawayCount: Int) {
        self.count = count
        self.p50 = p50
        self.p95 = p95
        self.p99 = p99
        self.maxCost = maxCost
        self.runawayThreshold = runawayThreshold
        self.runawayCount = runawayCount
    }
}

/// One row from `pull_requests` ⋈ `pr_cost_rollups`.
public struct LedgerPRCost: Sendable, Identifiable, Hashable {
    public let cwd: String                // absolute path or relative repo key
    public let prNumber: Int
    public let branch: String
    public let state: String              // "open" | "merged" | ...
    public let totalCostUSD: Double
    public let aiLinesAdded: Int
    public let aiLinesRemoved: Int
    public let sessionCount: Int
    public let commitCount: Int
    public let lastComputedAt: Date

    public var id: String { "\(cwd)#\(prNumber)" }

    public init(
        cwd: String, prNumber: Int, branch: String, state: String,
        totalCostUSD: Double, aiLinesAdded: Int, aiLinesRemoved: Int,
        sessionCount: Int, commitCount: Int, lastComputedAt: Date
    ) {
        self.cwd = cwd
        self.prNumber = prNumber
        self.branch = branch
        self.state = state
        self.totalCostUSD = totalCostUSD
        self.aiLinesAdded = aiLinesAdded
        self.aiLinesRemoved = aiLinesRemoved
        self.sessionCount = sessionCount
        self.commitCount = commitCount
        self.lastComputedAt = lastComputedAt
    }

    /// Final path component as the repo "short name".
    public var repoShortName: String {
        URL(fileURLWithPath: cwd).lastPathComponent
    }

    /// Cost per AI line added, nil when no AI lines.
    public var costPerLine: Double? {
        guard aiLinesAdded > 0 else { return nil }
        return totalCostUSD / Double(aiLinesAdded)
    }
}
