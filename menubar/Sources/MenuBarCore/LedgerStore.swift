import Foundation
import SQLite3

// SQLite C const that Swift can't import directly because the value is a
// macro pointing at -1. Re-declare with the same magic value.
private let SQLITE_TRANSIENT = unsafeBitCast(
    OpaquePointer(bitPattern: -1), to: sqlite3_destructor_type.self
)

private struct LedgerSQLError: Error { let message: String }

/// Read-only reader for `~/.cc-ledger/ledger.db`, populated by the Rust
/// cc-ledger CLI's hooks. Menubar is a strict reader — never writes.
///
/// Honors `CC_LEDGER_HOME` for parity with the Rust side.
public actor LedgerStore {
    /// Schema version this build knows how to read. Bump in lockstep with the
    /// Rust side's `SCHEMA_VERSION` (see `cc-ledger/src/store/mod.rs`).
    public static let supportedSchemaVersion: Int = 4

    public private(set) var status: LedgerStatus = .unknown
    private var db: OpaquePointer?

    public init() {}

    // No deinit: Swift 6 forbids non-Sendable access from a nonisolated
    // deinit. The DB handle is a process-lifetime resource — the OS closes
    // the fd on exit. A `disconnect()` method exists for the rare case where
    // a caller wants to release explicitly.
    public func disconnect() { closeDB() }

    /// Default DB location. `CC_LEDGER_HOME` wins if set; otherwise `~/.cc-ledger/`.
    public static func defaultDBPath() -> URL {
        let env = ProcessInfo.processInfo.environment
        if let home = env["CC_LEDGER_HOME"], !home.isEmpty {
            return URL(fileURLWithPath: home).appendingPathComponent("ledger.db")
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".cc-ledger/ledger.db")
    }

    /// Returns the current status. Cheap on repeat calls because the open
    /// connection is reused; if the DB is absent or fails the version check
    /// we close any prior handle and remember the failure.
    @discardableResult
    public func refreshStatus() -> LedgerStatus {
        let url = Self.defaultDBPath()
        guard FileManager.default.fileExists(atPath: url.path) else {
            closeDB()
            status = .absent
            return status
        }
        // Reopen each refresh: immutable=1 snapshots at open time, so to pick
        // up new writes we close-and-reopen. Cost is microseconds.
        closeDB()
        switch openReadOnly(at: url) {
        case .success(let handle): db = handle
        case .failure(let err):
            closeDB()
            status = .readError(err.message)
            return status
        }
        // Schema-version check — if we don't recognize the value, bail.
        switch readSchemaVersion() {
        case .success(let v):
            if v > Self.supportedSchemaVersion {
                closeDB()
                status = .versionMismatch(actual: v, expected: Self.supportedSchemaVersion)
            } else {
                status = .available(version: v)
            }
        case .failure(let err):
            closeDB()
            status = .readError(err.message)
        }
        return status
    }

    public func activityCategories(sinceHours: Double = 30 * 24) -> [LedgerActivityCategory] {
        guard case .available = refreshStatus(), let db else { return [] }
        let since = Date().addingTimeInterval(-sinceHours * 3600)
        let sinceMs = Int64(since.timeIntervalSince1970 * 1000)
        let sql = """
            SELECT category, COUNT(*), SUM(cost_usd_api_equiv),
                   SUM(input_tokens + output_tokens + cache_read_tokens
                       + cache_write_5m_tokens + cache_write_1h_tokens)
            FROM agent_turns
            WHERE started_at_ms >= ?1 AND category IS NOT NULL
            GROUP BY category
            ORDER BY SUM(cost_usd_api_equiv) DESC
        """
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else { return [] }
        defer { sqlite3_finalize(stmt) }
        sqlite3_bind_int64(stmt, 1, sinceMs)

        var rows: [LedgerActivityCategory] = []
        while sqlite3_step(stmt) == SQLITE_ROW {
            guard let cstr = sqlite3_column_text(stmt, 0) else { continue }
            let category = String(cString: cstr)
            let turns    = Int(sqlite3_column_int64(stmt, 1))
            let cost     = sqlite3_column_double(stmt, 2)
            let tokens   = Int(sqlite3_column_int64(stmt, 3))
            rows.append(.init(category: category, turnCount: turns,
                              costUSD: cost, totalTokens: tokens))
        }
        return rows
    }

    /// PR cost distribution across all non-zero rollups. Nil when fewer than
    /// 5 PRs exist (percentiles below that are noise). Threshold for the
    /// runaway badge is p95 — high enough to skip noise, low enough that a
    /// PR ≥ 20× the typical cost still shows up.
    public func prCostStats() -> LedgerPRCostStats? {
        guard case .available = refreshStatus(), let db else { return nil }
        let sql = """
            SELECT total_cost_usd_api_equiv FROM pr_cost_rollups
            WHERE total_cost_usd_api_equiv > 0
            ORDER BY total_cost_usd_api_equiv
        """
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else { return nil }
        defer { sqlite3_finalize(stmt) }

        var costs: [Double] = []
        while sqlite3_step(stmt) == SQLITE_ROW {
            costs.append(sqlite3_column_double(stmt, 0))
        }
        guard costs.count >= 5 else { return nil }

        // Linear-interpolation percentile (R-7), sorted asc.
        func pct(_ p: Double) -> Double {
            let idx = (p / 100.0) * Double(costs.count - 1)
            let lo = Int(idx.rounded(.down))
            let hi = Int(idx.rounded(.up))
            if lo == hi { return costs[lo] }
            let t = idx - Double(lo)
            return costs[lo] * (1 - t) + costs[hi] * t
        }
        let p95 = pct(95)
        return LedgerPRCostStats(
            count: costs.count,
            p50: pct(50),
            p95: p95,
            p99: pct(99),
            maxCost: costs.last ?? 0,
            runawayThreshold: p95
        )
    }

    /// Session cost distribution across all sessions with > 0 cost in
    /// `agent_turns`. Nil when fewer than 5 sessions exist (percentiles
    /// below that are noise). Runaway threshold is `p95` to mirror the PR
    /// stats — same noise-vs-signal trade-off.
    public func sessionCostStats() -> LedgerSessionCostStats? {
        guard case .available = refreshStatus(), let db else { return nil }
        let sql = """
            SELECT SUM(cost_usd_api_equiv) AS total
              FROM agent_turns
             WHERE started_at_ms IS NOT NULL
             GROUP BY session_id
            HAVING total > 0
             ORDER BY total
        """
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else { return nil }
        defer { sqlite3_finalize(stmt) }

        var costs: [Double] = []
        while sqlite3_step(stmt) == SQLITE_ROW {
            costs.append(sqlite3_column_double(stmt, 0))
        }
        guard costs.count >= 5 else { return nil }

        // Linear-interpolation percentile (R-7), sorted asc — same as prCostStats.
        func pct(_ p: Double) -> Double {
            let idx = (p / 100.0) * Double(costs.count - 1)
            let lo = Int(idx.rounded(.down))
            let hi = Int(idx.rounded(.up))
            if lo == hi { return costs[lo] }
            let t = idx - Double(lo)
            return costs[lo] * (1 - t) + costs[hi] * t
        }
        let p95 = pct(95)
        let runawayCount = costs.filter { $0 > p95 }.count
        return LedgerSessionCostStats(
            count: costs.count,
            p50: pct(50),
            p95: p95,
            p99: pct(99),
            maxCost: costs.last ?? 0,
            runawayThreshold: p95,
            runawayCount: runawayCount
        )
    }

    /// Activity-category breakdown for a single PR. Joins through
    /// `pr_commits` → `attributions` → `agent_turns` to find the
    /// agent-turn rows whose sessions touched any commit in this PR,
    /// then groups by category. Ordered by cost DESC.
    public func categoryBreakdownForPR(cwd: String, prNumber: Int) -> [LedgerActivityCategory] {
        guard case .available = refreshStatus(), let db else { return [] }
        let sql = """
            SELECT at.category,
                   COUNT(*),
                   SUM(at.cost_usd_api_equiv),
                   SUM(at.input_tokens + at.output_tokens + at.cache_read_tokens
                       + at.cache_write_5m_tokens + at.cache_write_1h_tokens)
            FROM agent_turns at
            WHERE at.category IS NOT NULL
              AND at.session_id IN (
                  SELECT DISTINCT a.session_id
                  FROM attributions a
                  WHERE a.cwd = ?1
                    AND a.commit_sha IN (
                        SELECT pc.commit_sha
                        FROM pr_commits pc
                        WHERE pc.cwd = ?1 AND pc.pr_number = ?2
                    )
              )
            GROUP BY at.category
            ORDER BY SUM(at.cost_usd_api_equiv) DESC
        """
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else { return [] }
        defer { sqlite3_finalize(stmt) }
        sqlite3_bind_text(stmt, 1, cwd, -1, SQLITE_TRANSIENT)
        sqlite3_bind_int(stmt, 2, Int32(prNumber))

        var rows: [LedgerActivityCategory] = []
        while sqlite3_step(stmt) == SQLITE_ROW {
            guard let cstr = sqlite3_column_text(stmt, 0) else { continue }
            let category = String(cString: cstr)
            let turns    = Int(sqlite3_column_int64(stmt, 1))
            let cost     = sqlite3_column_double(stmt, 2)
            let tokens   = Int(sqlite3_column_int64(stmt, 3))
            rows.append(.init(category: category, turnCount: turns,
                              costUSD: cost, totalTokens: tokens))
        }
        return rows
    }

    public func recentPRCosts(limit: Int = 10) -> [LedgerPRCost] {
        guard case .available = refreshStatus(), let db else { return [] }
        let sql = """
            SELECT p.cwd, p.pr_number, p.branch, p.state,
                   COALESCE(r.total_cost_usd_api_equiv, 0),
                   COALESCE(r.ai_lines_added, 0),
                   COALESCE(r.ai_lines_removed, 0),
                   COALESCE(r.session_count, 0),
                   COALESCE(r.commit_count, 0),
                   COALESCE(r.last_computed_at_ms, 0)
            FROM pull_requests p
            JOIN pr_cost_rollups r
              ON r.cwd = p.cwd AND r.pr_number = p.pr_number
            ORDER BY r.last_computed_at_ms DESC
            LIMIT ?1
        """
        var stmt: OpaquePointer?
        guard sqlite3_prepare_v2(db, sql, -1, &stmt, nil) == SQLITE_OK else { return [] }
        defer { sqlite3_finalize(stmt) }
        sqlite3_bind_int(stmt, 1, Int32(limit))

        var rows: [LedgerPRCost] = []
        while sqlite3_step(stmt) == SQLITE_ROW {
            let cwd     = sqlite3_column_text(stmt, 0).map { String(cString: $0) } ?? ""
            let prNum   = Int(sqlite3_column_int64(stmt, 1))
            let branch  = sqlite3_column_text(stmt, 2).map { String(cString: $0) } ?? ""
            let state   = sqlite3_column_text(stmt, 3).map { String(cString: $0) } ?? "open"
            let cost    = sqlite3_column_double(stmt, 4)
            let added   = Int(sqlite3_column_int64(stmt, 5))
            let removed = Int(sqlite3_column_int64(stmt, 6))
            let sess    = Int(sqlite3_column_int64(stmt, 7))
            let commits = Int(sqlite3_column_int64(stmt, 8))
            let tsMs    = sqlite3_column_int64(stmt, 9)
            let ts      = Date(timeIntervalSince1970: TimeInterval(tsMs) / 1000)
            rows.append(.init(
                cwd: cwd, prNumber: prNum, branch: branch, state: state,
                totalCostUSD: cost, aiLinesAdded: added, aiLinesRemoved: removed,
                sessionCount: sess, commitCount: commits, lastComputedAt: ts
            ))
        }
        return rows
    }

    // MARK: - Internals

    private func openReadOnly(at url: URL) -> Result<OpaquePointer, LedgerSQLError> {
        // Sanity probe: can we even read a byte from the file? If this fails,
        // the failure is a permission / TCC issue, not a SQLite issue.
        do {
            let fh = try FileHandle(forReadingFrom: url)
            _ = try fh.read(upToCount: 16)
            try fh.close()
        } catch {
            return .failure(LedgerSQLError(message: "open file: \(error)"))
        }

        var handle: OpaquePointer?
        // URI form with immutable=1 tells SQLite the DB never changes from
        // this connection's perspective — disables WAL/SHM access entirely.
        // That's what we want: the writer (cc-ledger CLI hooks) keeps using
        // WAL freely, and we read snapshots without needing write access to
        // the dir for .wal-shm. We re-open the connection each refresh to
        // pick up new writes.
        let flags = SQLITE_OPEN_READONLY | SQLITE_OPEN_NOMUTEX | SQLITE_OPEN_URI
        let uri = "file://\(url.path)?immutable=1"
        let rc = sqlite3_open_v2(uri, &handle, flags, nil)
        guard rc == SQLITE_OK, let handle else {
            let msg = handle.flatMap { sqlite3_errmsg($0).map { String(cString: $0) } } ?? "sqlite3_open_v2 rc=\(rc)"
            if let h = handle { sqlite3_close_v2(h) }
            return .failure(LedgerSQLError(message: msg))
        }
        // 5s busy timeout matches the writer's pragma in cc-ledger/src/store/mod.rs.
        sqlite3_busy_timeout(handle, 5_000)
        return .success(handle)
    }

    private func readSchemaVersion() -> Result<Int, LedgerSQLError> {
        guard let db else { return .failure(LedgerSQLError(message: "no db")) }
        let sql = "SELECT value FROM schema_meta WHERE key='version'"
        var stmt: OpaquePointer?
        let rc = sqlite3_prepare_v2(db, sql, -1, &stmt, nil)
        guard rc == SQLITE_OK else {
            let msg = sqlite3_errmsg(db).map { String(cString: $0) } ?? "rc=\(rc)"
            let path = sqlite3_db_filename(db, "main").map { String(cString: $0) } ?? "?"
            return .failure(LedgerSQLError(message: "prepare schema_meta failed: \(msg) — db path: \(path)"))
        }
        defer { sqlite3_finalize(stmt) }
        guard sqlite3_step(stmt) == SQLITE_ROW else {
            return .failure(LedgerSQLError(message: "schema_meta row missing"))
        }
        guard let cstr = sqlite3_column_text(stmt, 0) else {
            return .failure(LedgerSQLError(message: "schema_meta value null"))
        }
        let raw = String(cString: cstr)
        guard let v = Int(raw) else {
            return .failure(LedgerSQLError(message: "schema_meta value not int: \(raw)"))
        }
        return .success(v)
    }

    private func closeDB() {
        if let db {
            sqlite3_close_v2(db)
            self.db = nil
        }
    }
}
