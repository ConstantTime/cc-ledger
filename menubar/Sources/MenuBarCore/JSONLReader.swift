import Foundation

public struct DayUsage: Sendable, Identifiable, Hashable {
    public let dayKey: String  // "YYYY-MM-DD" in local time
    public let inputTokens: Int
    public let outputTokens: Int
    public let cacheReadTokens: Int
    public let cacheCreationTokens: Int
    public let totalCostUSD: Double
    public let perModel: [String: ModelDay]
    public let thinkingLengths: [Int]
    public let toolUseCounts: [String: Int]

    public var id: String { dayKey }

    public var totalTokens: Int {
        inputTokens + outputTokens + cacheReadTokens + cacheCreationTokens
    }
}

public struct ModelDay: Sendable, Hashable {
    public let inputTokens: Int
    public let outputTokens: Int
    public let cacheReadTokens: Int
    public let cacheCreationTokens: Int
    public let costUSD: Double
}

public struct UsageHistory: Sendable {
    public let days: [DayUsage]
    public let totalCostUSD: Double
    public let totalTokens: Int

    public var today: DayUsage? {
        let key = JSONLReader.dayKey(for: Date())
        return days.first { $0.dayKey == key }
    }
}

public struct HourUsage: Sendable, Identifiable, Hashable {
    public let hourKey: String      // "YYYY-MM-DD HH" in local time
    public let hourStart: Date
    public let inputTokens: Int
    public let outputTokens: Int
    public let cacheReadTokens: Int
    public let cacheCreationTokens: Int
    public let totalCostUSD: Double
    public let perModel: [String: ModelDay]

    public var id: String { hourKey }

    public var totalTokens: Int {
        inputTokens + outputTokens + cacheReadTokens + cacheCreationTokens
    }
}

public struct UsageHistoryHourly: Sendable {
    public let hours: [HourUsage]
    public let totalCostUSD: Double
    public let totalTokens: Int
}

public struct ActiveSession: Sendable, Identifiable, Hashable {
    public let sessionId: String          // UUID from <sessionId>.jsonl
    public let projectPath: String        // "/Users/win11905/code/menubar"
    public let projectName: String        // "menubar"
    public let firstActivityAt: Date
    public let lastActivityAt: Date
    public let turnCount: Int
    public let costUSD: Double
    public let inputTokens: Int
    public let outputTokens: Int
    public let cacheReadTokens: Int
    public let cacheCreationTokens: Int
    public let perModel: [String: ModelDay]
    public let toolCounts: [String: Int]
    public let isSubagent: Bool

    public var id: String { sessionId }

    public var totalTokens: Int {
        inputTokens + outputTokens + cacheReadTokens + cacheCreationTokens
    }
}

public actor JSONLReader {
    public init() {}

    /// Default roots: ~/.claude/projects and ~/.config/claude/projects, plus
    /// any directory listed in CLAUDE_CONFIG_DIR (comma-separated).
    public static func defaultRoots() -> [URL] {
        var roots: [URL] = []
        let env = ProcessInfo.processInfo.environment["CLAUDE_CONFIG_DIR"] ?? ""
        if !env.isEmpty {
            for part in env.split(separator: ",") {
                let raw = String(part).trimmingCharacters(in: .whitespacesAndNewlines)
                guard !raw.isEmpty else { continue }
                let url = URL(fileURLWithPath: raw)
                if url.lastPathComponent == "projects" {
                    roots.append(url)
                } else {
                    roots.append(url.appendingPathComponent("projects", isDirectory: true))
                }
            }
        } else {
            let home = FileManager.default.homeDirectoryForCurrentUser
            roots.append(home.appendingPathComponent(".claude/projects", isDirectory: true))
            roots.append(home.appendingPathComponent(".config/claude/projects", isDirectory: true))
        }
        return roots
    }

    public func scan(roots: [URL] = JSONLReader.defaultRoots(), days: Int = 30) -> UsageHistory {
        let calendar = Calendar.current
        let now = Date()
        let oldestDate = calendar.date(byAdding: .day, value: -days, to: now) ?? now
        let parsed = loadParsedFiles(roots: roots, walkSinceDate: oldestDate)

        // Merge per-day buckets across all parsed files.
        var bucket: [String: [String: ModelTokens]] = [:]
        var thinkingByDay: [String: [Int]] = [:]
        var toolByDay: [String: [String: Int]] = [:]

        for pf in parsed {
            for (day, models) in pf.dayBuckets {
                var existing = bucket[day] ?? [:]
                for (model, tokens) in models {
                    var acc = existing[model] ?? ModelTokens()
                    acc.input         += tokens.input
                    acc.output        += tokens.output
                    acc.cacheRead     += tokens.cacheRead
                    acc.cacheCreation += tokens.cacheCreation
                    existing[model] = acc
                }
                bucket[day] = existing
            }
            for (day, lengths) in pf.thinkingLengthsByDay {
                thinkingByDay[day, default: []].append(contentsOf: lengths)
            }
            for (day, tools) in pf.toolByDay {
                var existing = toolByDay[day] ?? [:]
                for (name, count) in tools {
                    existing[name, default: 0] += count
                }
                toolByDay[day] = existing
            }
        }

        var days: [DayUsage] = []
        var totalCost = 0.0
        var totalTokens = 0
        let allDayKeys = Set(bucket.keys)
            .union(thinkingByDay.keys)
            .union(toolByDay.keys)

        for dayKey in allDayKeys {
            var dayInput = 0, dayOutput = 0, dayCacheRead = 0, dayCacheCreation = 0
            var dayCost = 0.0
            var modelMap: [String: ModelDay] = [:]
            if let perModel = bucket[dayKey] {
                for (model, tokens) in perModel {
                    let cost = Pricing.cost(
                        model: model,
                        inputTokens: tokens.input,
                        outputTokens: tokens.output,
                        cacheReadTokens: tokens.cacheRead,
                        cacheCreationTokens: tokens.cacheCreation
                    )
                    modelMap[model] = ModelDay(
                        inputTokens: tokens.input,
                        outputTokens: tokens.output,
                        cacheReadTokens: tokens.cacheRead,
                        cacheCreationTokens: tokens.cacheCreation,
                        costUSD: cost
                    )
                    dayInput         += tokens.input
                    dayOutput        += tokens.output
                    dayCacheRead     += tokens.cacheRead
                    dayCacheCreation += tokens.cacheCreation
                    dayCost          += cost
                }
            }
            days.append(DayUsage(
                dayKey: dayKey,
                inputTokens: dayInput,
                outputTokens: dayOutput,
                cacheReadTokens: dayCacheRead,
                cacheCreationTokens: dayCacheCreation,
                totalCostUSD: dayCost,
                perModel: modelMap,
                thinkingLengths: thinkingByDay[dayKey] ?? [],
                toolUseCounts: toolByDay[dayKey] ?? [:]
            ))
            totalCost   += dayCost
            totalTokens += dayInput + dayOutput + dayCacheRead + dayCacheCreation
        }

        days.sort { $0.dayKey < $1.dayKey }
        return UsageHistory(days: days, totalCostUSD: totalCost, totalTokens: totalTokens)
    }

    /// Hourly aggregation over the last `hours` hours (rolling). Reuses the
    /// same parsed-file cache as `scan()`, then filters hour buckets by recency.
    public func scanHourly(roots: [URL] = JSONLReader.defaultRoots(), hours: Int = 24) -> UsageHistoryHourly {
        let now = Date()
        let oldestDate = now.addingTimeInterval(-Double(hours) * 3600)
        let parsed = loadParsedFiles(roots: roots, walkSinceDate: oldestDate)

        var bucket: [String: [String: ModelTokens]] = [:]
        var hourStarts: [String: Date] = [:]

        for pf in parsed {
            for (hourKey, hb) in pf.hourBuckets {
                guard hb.hourStart >= oldestDate else { continue }
                if hourStarts[hourKey] == nil {
                    hourStarts[hourKey] = hb.hourStart
                }
                var existing = bucket[hourKey] ?? [:]
                for (model, tokens) in hb.perModel {
                    var acc = existing[model] ?? ModelTokens()
                    acc.input         += tokens.input
                    acc.output        += tokens.output
                    acc.cacheRead     += tokens.cacheRead
                    acc.cacheCreation += tokens.cacheCreation
                    existing[model] = acc
                }
                bucket[hourKey] = existing
            }
        }

        var hoursArr: [HourUsage] = []
        var totalCost = 0.0
        var totalTokens = 0

        for (hourKey, perModel) in bucket {
            var hIn = 0, hOut = 0, hCR = 0, hCC = 0
            var hCost = 0.0
            var modelMap: [String: ModelDay] = [:]
            for (model, tokens) in perModel {
                let cost = Pricing.cost(
                    model: model,
                    inputTokens: tokens.input,
                    outputTokens: tokens.output,
                    cacheReadTokens: tokens.cacheRead,
                    cacheCreationTokens: tokens.cacheCreation
                )
                modelMap[model] = ModelDay(
                    inputTokens: tokens.input,
                    outputTokens: tokens.output,
                    cacheReadTokens: tokens.cacheRead,
                    cacheCreationTokens: tokens.cacheCreation,
                    costUSD: cost
                )
                hIn   += tokens.input
                hOut  += tokens.output
                hCR   += tokens.cacheRead
                hCC   += tokens.cacheCreation
                hCost += cost
            }
            hoursArr.append(HourUsage(
                hourKey: hourKey,
                hourStart: hourStarts[hourKey] ?? now,
                inputTokens: hIn,
                outputTokens: hOut,
                cacheReadTokens: hCR,
                cacheCreationTokens: hCC,
                totalCostUSD: hCost,
                perModel: modelMap
            ))
            totalCost   += hCost
            totalTokens += hIn + hOut + hCR + hCC
        }
        hoursArr.sort { $0.hourKey < $1.hourKey }
        return UsageHistoryHourly(hours: hoursArr, totalCostUSD: totalCost, totalTokens: totalTokens)
    }

    /// Sessions with any activity in the last `withinHours`, sorted most-recent
    /// first. Reuses the on-disk parse cache so this is near-zero-cost when hot.
    public func activeSessions(
        roots: [URL] = JSONLReader.defaultRoots(),
        withinHours: Double = 1.0,
        maxResults: Int = 10
    ) -> [ActiveSession] {
        let cutoff = Date().addingTimeInterval(-withinHours * 3600)
        let parsed = loadParsedFiles(roots: roots, walkSinceDate: cutoff)

        return parsed
            .compactMap { pf -> ActiveSession? in
                guard let last = pf.lastActivityAt, last >= cutoff else { return nil }
                guard let sid = pf.sessionId else { return nil }
                // Prefer cwd captured from JSONL turns — authoritative for paths
                // that don't round-trip through slug encoding (e.g. worktrees,
                // dot-prefixed dirs, hyphenated component names). Fall back to
                // re-deriving from the file URL so stale caches with old (wrong)
                // "subagents" projectPath still self-heal.
                let path: String
                if let cwd = pf.cwd, !cwd.isEmpty {
                    path = cwd
                } else {
                    let fileURL = URL(fileURLWithPath: pf.path)
                    let slug = Self.projectSlug(for: fileURL, isSubagent: pf.isSubagent)
                    path = Self.decodeProjectSlug(slug)
                }
                let name = (path as NSString).lastPathComponent

                // Sum all-time tokens + per-model + tools across the file's buckets.
                var input = 0, output = 0, cR = 0, cC = 0
                var modelMap: [String: ModelDay] = [:]
                for (_, models) in pf.dayBuckets {
                    for (model, t) in models {
                        input  += t.input
                        output += t.output
                        cR     += t.cacheRead
                        cC     += t.cacheCreation
                        let cost = Pricing.cost(
                            model: model,
                            inputTokens: t.input,
                            outputTokens: t.output,
                            cacheReadTokens: t.cacheRead,
                            cacheCreationTokens: t.cacheCreation
                        )
                        if var existing = modelMap[model] {
                            existing = ModelDay(
                                inputTokens: existing.inputTokens + t.input,
                                outputTokens: existing.outputTokens + t.output,
                                cacheReadTokens: existing.cacheReadTokens + t.cacheRead,
                                cacheCreationTokens: existing.cacheCreationTokens + t.cacheCreation,
                                costUSD: existing.costUSD + cost
                            )
                            modelMap[model] = existing
                        } else {
                            modelMap[model] = ModelDay(
                                inputTokens: t.input,
                                outputTokens: t.output,
                                cacheReadTokens: t.cacheRead,
                                cacheCreationTokens: t.cacheCreation,
                                costUSD: cost
                            )
                        }
                    }
                }
                var tools: [String: Int] = [:]
                for (_, m) in pf.toolByDay {
                    for (k, v) in m { tools[k, default: 0] += v }
                }

                return ActiveSession(
                    sessionId: sid,
                    projectPath: path,
                    projectName: name.isEmpty ? sid.prefix(8).description : name,
                    firstActivityAt: pf.firstActivityAt ?? last,
                    lastActivityAt: last,
                    turnCount: pf.turnCount,
                    costUSD: pf.totalCostUSD,
                    inputTokens: input,
                    outputTokens: output,
                    cacheReadTokens: cR,
                    cacheCreationTokens: cC,
                    perModel: modelMap,
                    toolCounts: tools,
                    isSubagent: pf.isSubagent
                )
            }
            .sorted { $0.lastActivityAt > $1.lastActivityAt }
            .prefix(maxResults)
            .map { $0 }
    }

    // MARK: - internals

    private struct Row {
        let timestamp: Date
        let model: String
        let inputTokens: Int
        let outputTokens: Int
        let cacheReadTokens: Int
        let cacheCreationTokens: Int
        let thinkingLengths: [Int]
        let toolNames: [String]
        let messageId: String?
        let requestId: String?
        let isSidechain: Bool
    }

    /// Walks JSONL files under `roots` filtered by mtime, returning a `ParsedFile`
    /// for each. Hits the on-disk `ScanCache` when (path, mtime, size) matches —
    /// otherwise parses fresh, updates the cache, and persists.
    private func loadParsedFiles(roots: [URL], walkSinceDate: Date) -> [ParsedFile] {
        var cache = ScanCacheStore.load()
        var result: [ParsedFile] = []
        var alivePaths: Set<String> = []
        var dirty = false

        let fm = FileManager.default
        for root in roots {
            guard fm.fileExists(atPath: root.path),
                  let enumerator = fm.enumerator(
                    at: root,
                    includingPropertiesForKeys: [.contentModificationDateKey, .fileSizeKey]
                  )
            else { continue }

            for case let url as URL in enumerator {
                guard url.pathExtension.lowercased() == "jsonl" else { continue }
                let resourceValues = try? url.resourceValues(
                    forKeys: [.contentModificationDateKey, .fileSizeKey]
                )
                guard let mtime = resourceValues?.contentModificationDate else { continue }
                let size = Int64(resourceValues?.fileSize ?? 0)

                // Files older than the walk window: keep the cache entry if present
                // (so we don't churn-prune it across scans), but don't return it.
                if mtime < walkSinceDate {
                    if cache.files[url.path] != nil {
                        alivePaths.insert(url.path)
                    }
                    continue
                }

                alivePaths.insert(url.path)

                if let cached = cache.files[url.path],
                   abs(cached.mtime.timeIntervalSince(mtime)) < 0.001,
                   cached.size == size {
                    result.append(cached)
                    continue
                }

                let parsed = parseFile(url: url, mtime: mtime, size: size)
                cache.files[url.path] = parsed
                result.append(parsed)
                dirty = true
            }
        }

        // Prune entries for files no longer present on disk.
        if cache.files.keys.contains(where: { !alivePaths.contains($0) }) {
            cache.files = cache.files.filter { alivePaths.contains($0.key) }
            dirty = true
        }

        if dirty { ScanCacheStore.save(cache) }
        return result
    }

    /// Reads a single JSONL file end-to-end and returns its aggregated buckets.
    /// Streaming/replay rows often appear multiple times with identical token
    /// counts — we dedupe by (messageId, requestId) keeping the last occurrence.
    /// Per-file session metadata (sessionId, projectPath, first/lastActivityAt,
    /// turnCount, totalCostUSD) is computed in the same pass.
    private func parseFile(url: URL, mtime: Date, size: Int64) -> ParsedFile {
        var dayBuckets: [String: [String: ModelTokens]] = [:]
        var hourBuckets: [String: HourBucket] = [:]
        var thinkingLengthsByDay: [String: [Int]] = [:]
        var toolByDay: [String: [String: Int]] = [:]

        let sessionId = url.deletingPathExtension().lastPathComponent
        let isSubagent = url.path.contains("/subagents/")
        let projectSlug = Self.projectSlug(for: url, isSubagent: isSubagent)
        let projectPath = Self.decodeProjectSlug(projectSlug)

        guard let data = try? Data(contentsOf: url) else {
            return ParsedFile(
                path: url.path, mtime: mtime, size: size,
                dayBuckets: [:], hourBuckets: [:],
                thinkingLengthsByDay: [:], toolByDay: [:],
                sessionId: sessionId, projectPath: projectPath,
                cwd: nil,
                firstActivityAt: nil, lastActivityAt: nil,
                turnCount: 0, totalCostUSD: 0,
                isSubagent: isSubagent
            )
        }

        // Pass 1: parse all rows. Dedupe assistant rows by (messageId, requestId);
        // unkeyed rows (legacy or missing IDs) are kept verbatim. Also capture
        // the latest cwd seen — authoritative for the session's project path.
        var keyed: [String: Row] = [:]
        var unkeyed: [Row] = []
        var latestCwd: String? = nil
        data.withUnsafeBytes { raw in
            let buffer = raw.bindMemory(to: UInt8.self)
            var start = 0
            for i in 0..<buffer.count {
                if buffer[i] == 0x0A {  // '\n'
                    if i > start {
                        let line = Data(
                            bytes: buffer.baseAddress!.advanced(by: start),
                            count: i - start
                        )
                        collect(line: line, keyed: &keyed, unkeyed: &unkeyed, latestCwd: &latestCwd)
                    }
                    start = i + 1
                }
            }
            if start < buffer.count {
                let line = Data(
                    bytes: buffer.baseAddress!.advanced(by: start),
                    count: buffer.count - start
                )
                collect(line: line, keyed: &keyed, unkeyed: &unkeyed, latestCwd: &latestCwd)
            }
        }

        // Pass 2: aggregate the deduped row set into per-day / per-hour buckets,
        // and accumulate the per-session summary fields.
        var firstActivity: Date?
        var lastActivity: Date?
        var totalCost: Double = 0
        let allRows = Array(keyed.values) + unkeyed
        for row in allRows {
            ingest(row: row,
                   into: &dayBuckets,
                   hourBuckets: &hourBuckets,
                   thinkingLengthsByDay: &thinkingLengthsByDay,
                   toolByDay: &toolByDay)

            if firstActivity == nil || row.timestamp < firstActivity! { firstActivity = row.timestamp }
            if lastActivity  == nil || row.timestamp > lastActivity!  { lastActivity  = row.timestamp }

            totalCost += Pricing.cost(
                model: row.model,
                inputTokens: row.inputTokens,
                outputTokens: row.outputTokens,
                cacheReadTokens: row.cacheReadTokens,
                cacheCreationTokens: row.cacheCreationTokens
            )
        }

        return ParsedFile(
            path: url.path, mtime: mtime, size: size,
            dayBuckets: dayBuckets, hourBuckets: hourBuckets,
            thinkingLengthsByDay: thinkingLengthsByDay, toolByDay: toolByDay,
            sessionId: sessionId, projectPath: projectPath,
            cwd: latestCwd,
            firstActivityAt: firstActivity, lastActivityAt: lastActivity,
            turnCount: allRows.count, totalCostUSD: totalCost,
            isSubagent: isSubagent
        )
    }

    /// Decodes a project slug (e.g. "-Users-win11905-code-menubar") back to a
    /// real filesystem path ("/Users/win11905/code/menubar"). Heuristic — slugs
    /// with literal dashes inside path components round-trip incorrectly,
    /// but it's good enough for display.
    private static func decodeProjectSlug(_ slug: String) -> String {
        guard slug.hasPrefix("-") else { return slug }
        return "/" + slug.dropFirst().replacingOccurrences(of: "-", with: "/")
    }

    /// Walks up from a session JSONL file to find its project slug directory.
    /// Subagent files live three levels deep
    /// (`<slug>/<parentSessionId>/subagents/<subagentId>.jsonl`),
    /// regular sessions one level (`<slug>/<sessionId>.jsonl`).
    private static func projectSlug(for fileURL: URL, isSubagent: Bool) -> String {
        if isSubagent {
            return fileURL
                .deletingLastPathComponent()  // .../subagents
                .deletingLastPathComponent()  // .../<parentSessionId>
                .deletingLastPathComponent()  // .../<slug>
                .lastPathComponent
        }
        return fileURL.deletingLastPathComponent().lastPathComponent
    }

    private func collect(
        line: Data,
        keyed: inout [String: Row],
        unkeyed: inout [Row],
        latestCwd: inout String?
    ) {
        let parsed = parseLine(line)
        if let c = parsed.cwd { latestCwd = c }
        guard let row = parsed.row else { return }
        // Sidechain rows are kept — they represent real subagent spend whose
        // tokens often only live in subagents/*.jsonl with no parent twin.
        // CodexBar keeps them too; cross-file dedup is not yet implemented but
        // the rare double-count is far smaller than the gap from dropping them.
        if let mid = row.messageId, let rid = row.requestId {
            keyed["\(mid):\(rid)"] = row   // in-file dedup, last write wins
        } else {
            unkeyed.append(row)
        }
    }

    private func ingest(
        row: Row,
        into dayBuckets: inout [String: [String: ModelTokens]],
        hourBuckets: inout [String: HourBucket],
        thinkingLengthsByDay: inout [String: [Int]],
        toolByDay: inout [String: [String: Int]]
    ) {
        let dayKey = Self.dayKey(for: row.timestamp)
        let hourKey = Self.hourKey(for: row.timestamp)

        // dayBuckets
        var dayMap = dayBuckets[dayKey] ?? [:]
        var dayAcc = dayMap[row.model] ?? ModelTokens()
        dayAcc.input         += row.inputTokens
        dayAcc.output        += row.outputTokens
        dayAcc.cacheRead     += row.cacheReadTokens
        dayAcc.cacheCreation += row.cacheCreationTokens
        dayMap[row.model] = dayAcc
        dayBuckets[dayKey] = dayMap

        // hourBuckets
        var hb = hourBuckets[hourKey] ?? HourBucket(
            hourStart: Self.hourStart(for: row.timestamp),
            perModel: [:]
        )
        var hourAcc = hb.perModel[row.model] ?? ModelTokens()
        hourAcc.input         += row.inputTokens
        hourAcc.output        += row.outputTokens
        hourAcc.cacheRead     += row.cacheReadTokens
        hourAcc.cacheCreation += row.cacheCreationTokens
        hb.perModel[row.model] = hourAcc
        hourBuckets[hourKey] = hb

        if !row.thinkingLengths.isEmpty {
            thinkingLengthsByDay[dayKey, default: []].append(contentsOf: row.thinkingLengths)
        }
        if !row.toolNames.isEmpty {
            var counts = toolByDay[dayKey] ?? [:]
            for name in row.toolNames {
                counts[name, default: 0] += 1
            }
            toolByDay[dayKey] = counts
        }
    }

    private func parseLine(_ data: Data) -> (row: Row?, cwd: String?) {
        // Pre-filter: only lines with usage tokens or a cwd field are interesting.
        // cwd comes from user/assistant turns and is authoritative for the project
        // path (slug decoding loses info for worktree paths).
        let hasUsage = data.range(of: Data("\"usage\"".utf8)) != nil
        let hasCwd = data.range(of: Data("\"cwd\"".utf8)) != nil
        guard hasUsage || hasCwd else { return (nil, nil) }

        guard let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return (nil, nil)
        }

        let cwdValue: String? = {
            guard let s = obj["cwd"] as? String, !s.isEmpty else { return nil }
            return s
        }()

        guard hasUsage,
              let message = obj["message"] as? [String: Any],
              let model = message["model"] as? String,
              let usage = message["usage"] as? [String: Any]
        else { return (nil, cwdValue) }

        let timestamp: Date = (obj["timestamp"] as? String).flatMap(parseISO8601) ?? Date()

        var thinkingLengths: [Int] = []
        var toolNames: [String] = []
        if let content = message["content"] as? [[String: Any]] {
            for block in content {
                guard let type = block["type"] as? String else { continue }
                switch type {
                case "thinking":
                    // Visible content first; fall back to signature length when redacted.
                    let text = (block["thinking"] as? String ?? "")
                        .trimmingCharacters(in: .whitespacesAndNewlines)
                    let isRedacted = text.isEmpty
                        || text.lowercased() == "[redacted]"
                        || text.lowercased() == "redacted"
                    if !isRedacted {
                        thinkingLengths.append(text.count)
                    } else if let sig = block["signature"] as? String, !sig.isEmpty {
                        thinkingLengths.append(sig.count)
                    }
                case "redacted_thinking":
                    if let sig = block["signature"] as? String, !sig.isEmpty {
                        thinkingLengths.append(sig.count)
                    } else if let data = block["data"] as? String, !data.isEmpty {
                        thinkingLengths.append(data.count)
                    }
                case "tool_use":
                    if let name = block["name"] as? String {
                        toolNames.append(name)
                    }
                default:
                    break
                }
            }
        }

        let row = Row(
            timestamp: timestamp,
            model: model,
            inputTokens: anyInt(usage["input_tokens"]),
            outputTokens: anyInt(usage["output_tokens"]),
            cacheReadTokens: anyInt(usage["cache_read_input_tokens"]),
            cacheCreationTokens: anyInt(usage["cache_creation_input_tokens"]),
            thinkingLengths: thinkingLengths,
            toolNames: toolNames,
            messageId: message["id"] as? String,
            requestId: obj["requestId"] as? String,
            isSidechain: (obj["isSidechain"] as? Bool) ?? false
        )
        return (row, cwdValue)
    }

    private func anyInt(_ value: Any?) -> Int {
        if let i = value as? Int { return i }
        if let d = value as? Double { return Int(d) }
        if let s = value as? String, let i = Int(s) { return i }
        return 0
    }

    private func parseISO8601(_ s: String) -> Date? {
        let f = ISO8601DateFormatter()
        f.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = f.date(from: s) { return d }
        f.formatOptions = [.withInternetDateTime]
        return f.date(from: s)
    }

    public static func dayKey(for date: Date) -> String {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.dateFormat = "yyyy-MM-dd"
        return f.string(from: date)
    }

    public static func hourKey(for date: Date) -> String {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.dateFormat = "yyyy-MM-dd HH"
        return f.string(from: date)
    }

    public static func hourStart(for date: Date) -> Date {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = .current
        let comps = cal.dateComponents([.year, .month, .day, .hour], from: date)
        return cal.date(from: comps) ?? date
    }
}
