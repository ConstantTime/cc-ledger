import Foundation
import MenuBarCore
import Observation

@MainActor
@Observable
final class UsageStore {
    var usage: ClaudeUsageResponse?
    var plan: ClaudePlan?
    var history: UsageHistory?
    var hourlyHistory: UsageHistoryHourly?
    var errorMessage: String?
    var isLoading = false
    var lastUpdated: Date?
    var fiveHourSamples: [UtilizationSample] = []
    var activeSessions: [ActiveSession] = []
    var ledgerStatus: LedgerStatus = .unknown
    var activityCategories: [LedgerActivityCategory] = []
    var activityWindowHours: Double = 30 * 24
    var recentPRs: [LedgerPRCost] = []
    var prCostStats: LedgerPRCostStats?
    var sessionCostStats: LedgerSessionCostStats?

    private let client = ClaudeUsageClient()
    private let reader = JSONLReader()
    private let ledger = LedgerStore()
    private var apiTask: Task<Void, Never>?
    private var localTask: Task<Void, Never>?
    private var activeTask: Task<Void, Never>?

    init() {
        fiveHourSamples = UtilizationSampleStore.load()
        startAutoRefresh()
    }

    func refresh() async {
        isLoading = true
        defer { isLoading = false }
        do {
            let creds = try ClaudeCredentialsStore.load()
            plan = creds.plan
            let result = try await client.fetch(accessToken: creds.accessToken)
            usage = result
            errorMessage = nil
            lastUpdated = Date()
            recordFiveHourSample(from: result)
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func recordFiveHourSample(from result: ClaudeUsageResponse) {
        guard let pct = result.fiveHour?.utilization,
              let resetsAt = result.fiveHour?.resetsAtDate else { return }
        let sessionStart = resetsAt.addingTimeInterval(-5 * 3600)
        var samples = fiveHourSamples
        samples.append(.init(timestamp: Date(), utilization: pct))
        samples = samples.filter { $0.timestamp >= sessionStart }
        fiveHourSamples = samples
        UtilizationSampleStore.save(samples)
    }

    func refreshHistory() async {
        // 365 days covers the heatmap's 1y window. Cheap on warm caches —
        // ScanCache only re-reads files whose mtime/size changed.
        let h = await reader.scan(days: 365)
        history = h
    }

    func refreshHourly() async {
        hourlyHistory = await reader.scanHourly(hours: 24)
    }

    func refreshActiveSessions() async {
        activeSessions = await reader.activeSessions(withinHours: 1.0, maxResults: 10)
    }

    /// Fetch the activity-category breakdown for a specific PR. Called
    /// lazily when the PR row's hover popover opens.
    func categoryBreakdownForPR(cwd: String, prNumber: Int) async -> [LedgerActivityCategory] {
        await ledger.categoryBreakdownForPR(cwd: cwd, prNumber: prNumber)
    }

    /// Pull the two cc-ledger-only views: activity categories (window-scoped)
    /// and recent PR costs. No-op when `~/.cc-ledger/ledger.db` is missing or
    /// its schema is newer than we know how to read.
    func refreshLedger() async {
        let status = await ledger.refreshStatus()
        ledgerStatus = status
        if case .available = status {
            activityCategories = await ledger.activityCategories(sinceHours: activityWindowHours)
            recentPRs = await ledger.recentPRCosts(limit: 10)
            prCostStats = await ledger.prCostStats()
            sessionCostStats = await ledger.sessionCostStats()
        } else {
            activityCategories = []
            recentPRs = []
            prCostStats = nil
            sessionCostStats = nil
        }
    }

    /// Re-fetch only the activity-categories panel for a new window. Called
    /// when the user picks a different range in the popover; avoids the cost
    /// of a full ledger refresh.
    func setActivityWindowHours(_ hours: Double) async {
        activityWindowHours = hours
        if case .available = ledgerStatus {
            activityCategories = await ledger.activityCategories(sinceHours: hours)
        }
    }

    /// API and JSONL scans run on independent cadences. The API endpoint
    /// (`/api/oauth/usage`) is rate-limited; local JSONL parsing is free, so
    /// we refresh it more often to keep the cost panels and burn chart fresh
    /// without hammering Anthropic.
    func startAutoRefresh(
        apiSeconds: TimeInterval = 300,     // 5 min — well under any reasonable rate limit
        localSeconds: TimeInterval = 60,
        activeSeconds: TimeInterval = 10    // Active sessions change minute-to-minute; cache makes this cheap
    ) {
        apiTask?.cancel()
        localTask?.cancel()
        activeTask?.cancel()

        apiTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                try? await Task.sleep(nanoseconds: UInt64(apiSeconds * 1_000_000_000))
            }
        }

        localTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refreshHistory()
                await self?.refreshHourly()
                await self?.refreshLedger()
                try? await Task.sleep(nanoseconds: UInt64(localSeconds * 1_000_000_000))
            }
        }

        activeTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refreshActiveSessions()
                try? await Task.sleep(nanoseconds: UInt64(activeSeconds * 1_000_000_000))
            }
        }
    }
}
