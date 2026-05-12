import AppKit
import Charts
import MenuBarCore
import SwiftUI

@main
struct MenuBarApp: App {
    @State private var store = UsageStore()

    init() {
        NSApplication.shared.setActivationPolicy(.accessory)
    }

    var body: some Scene {
        MenuBarExtra {
            UsageView(store: store)
        } label: {
            UsageLabel(store: store)
        }
        .menuBarExtraStyle(.window)
    }
}

extension Color {
    static let claudeOrange = Color(red: 0xD9 / 255, green: 0x77 / 255, blue: 0x57 / 255)
}

/// Daily token target (input + output + cache, all tokens consumed today).
/// 500M is the default. Tweak this constant to shift the goal.
let dailyGoalTokens: Int = 500_000_000

struct UsageLabel: View {
    let store: UsageStore

    var body: some View {
        // Hard cap warning trumps everything — you always need to know.
        if let pct = store.usage?.fiveHour?.utilization, pct >= 90 {
            Text(String(format: "⚠ %.0f%%", pct))
        } else if let w = store.usage?.fiveHour,
                  let resetsAt = w.resetsAtDate,
                  let pct = w.utilization {
            // 5-hour mini burn chart + percentage. The chart is rasterized to
            // an NSImage because MenuBarExtra labels only reliably display
            // Text / Image — arbitrary SwiftUI views (Canvas, Path, ZStack)
            // get silently dropped or clipped.
            let sessionStart = resetsAt.addingTimeInterval(-5 * 3600)
            HStack(spacing: 4) {
                if let img = renderBurnChart(
                    samples: store.fiveHourSamples.filter { $0.timestamp >= sessionStart },
                    sessionStart: sessionStart,
                    resetsAt: resetsAt,
                    currentPct: pct
                ) {
                    Image(nsImage: img)
                }
                Text(String(format: "%.0f%%", pct))
                    .monospacedDigit()
            }
        } else if store.errorMessage != nil {
            Image(systemName: "exclamationmark.triangle")
        } else {
            Image(systemName: "circle.dotted")
        }
    }

    @MainActor
    private func renderBurnChart(
        samples: [UtilizationSample],
        sessionStart: Date,
        resetsAt: Date,
        currentPct: Double
    ) -> NSImage? {
        let chart = MiniBurnChart(
            samples: samples,
            sessionStart: sessionStart,
            resetsAt: resetsAt,
            currentPct: currentPct,
            now: Date()
        )
        let renderer = ImageRenderer(content: chart)
        renderer.scale = NSScreen.main?.backingScaleFactor ?? 2
        return renderer.nsImage
    }
}

/// Tiny burn chart for the menubar label. Same visual language as the popover
/// chart — dashed gray pace line, claudeOrange filled area + line for actual.
/// Uses Path-based shapes (not Canvas) because MenuBarExtra label rendering
/// doesn't honor Canvas drawing.
struct MiniBurnChart: View {
    let samples: [UtilizationSample]
    let sessionStart: Date
    let resetsAt: Date
    let currentPct: Double
    let now: Date

    private static let chartSize = CGSize(width: 50, height: 14)

    /// Anchored samples: always include (start, 0%) and (now, currentPct) so
    /// the line is meaningful even with sparse sample data.
    private var anchored: [(Date, Double)] {
        var pts: [(Date, Double)] = [(sessionStart, 0)]
        pts.append(contentsOf: samples.map { ($0.timestamp, $0.utilization) })
        pts.append((now, currentPct))
        return pts
    }

    private func point(_ t: Date, _ y: Double, in size: CGSize) -> CGPoint {
        let total = max(1, resetsAt.timeIntervalSince(sessionStart))
        let x = CGFloat(t.timeIntervalSince(sessionStart) / total) * size.width
        let yy = (1 - CGFloat(min(max(y, 0), 100) / 100)) * size.height
        return CGPoint(x: max(0, min(size.width, x)), y: yy)
    }

    var body: some View {
        let size = Self.chartSize
        ZStack {
            // 1. Dashed target diagonal: (start, 0%) → (reset, 100%)
            Path { p in
                p.move(to: CGPoint(x: 0, y: size.height))
                p.addLine(to: CGPoint(x: size.width, y: 0))
            }
            .stroke(Color.gray.opacity(0.45),
                    style: StrokeStyle(lineWidth: 0.8, dash: [2, 2]))

            // 2. Filled gradient area under the actual line
            Path { p in
                p.move(to: CGPoint(x: 0, y: size.height))
                for (t, y) in anchored { p.addLine(to: point(t, y, in: size)) }
                p.addLine(to: CGPoint(x: point(now, currentPct, in: size).x, y: size.height))
                p.closeSubpath()
            }
            .fill(
                LinearGradient(
                    colors: [
                        Color.claudeOrange.opacity(0.40),
                        Color.claudeOrange.opacity(0.05)
                    ],
                    startPoint: .top,
                    endPoint: .bottom
                )
            )

            // 3. Solid claudeOrange line on top
            Path { p in
                for (i, pair) in anchored.enumerated() {
                    let pt = point(pair.0, pair.1, in: size)
                    if i == 0 { p.move(to: pt) } else { p.addLine(to: pt) }
                }
            }
            .stroke(Color.claudeOrange, lineWidth: 1.2)
        }
        .frame(width: size.width, height: size.height)
    }
}

func formatCompactTokens(_ n: Int) -> String {
    if n >= 1_000_000_000 { return String(format: "%.1fB", Double(n) / 1_000_000_000) }
    if n >= 1_000_000     { return String(format: "%.0fM", Double(n) / 1_000_000) }
    if n >= 1_000         { return String(format: "%.0fK", Double(n) / 1_000) }
    return "\(n)"
}

enum HistoryWindow: String, CaseIterable, Identifiable {
    case hours24 = "24h"
    case days30  = "30d"

    var id: String { rawValue }

    var isHourly: Bool { self == .hours24 }

    var bucketCount: Int {
        switch self {
        case .hours24: return 24
        case .days30:  return 30
        }
    }

    var headlineLabel: String {
        switch self {
        case .hours24: return "Last 24h"
        case .days30:  return "30 days"
        }
    }
}

enum ActivityWindow: String, CaseIterable, Identifiable {
    case hours24 = "24h"
    case days7   = "7d"
    case days30  = "30d"

    var id: String { rawValue }

    var sinceHours: Double {
        switch self {
        case .hours24: return 24
        case .days7:   return 7 * 24
        case .days30:  return 30 * 24
        }
    }

    var totalLabel: String {
        switch self {
        case .hours24: return "24h"
        case .days7:   return "7d"
        case .days30:  return "30d"
        }
    }
}

enum ChartStyle: String, CaseIterable, Identifiable {
    case bars
    case heatmap

    var id: String { rawValue }

    var icon: String {
        switch self {
        case .bars:    return "chart.bar.fill"
        case .heatmap: return "square.grid.3x3.fill"
        }
    }
}

/// Heatmap is fixed at a 6-month window (26 weeks). Constants live here so
/// the heatmap view + headline + bar synthesizer share one source of truth.
enum HeatmapConfig {
    static let weeks: Int = 26
    static let cellSide: CGFloat = 8
    static let cellGap: CGFloat = 1
    static let headlineLabel = "180 days"
}

struct CostBar: Identifiable, Hashable {
    let key: String
    let displayLabel: String
    let cost: Double
    let totalTokens: Int
    let perModel: [String: ModelDay]
    var id: String { key }
}

struct UsageView: View {
    @Bindable var store: UsageStore
    @State private var selectedDayKey: String?
    @State private var window: HistoryWindow = .days30
    @State private var growProgress: Double = 0
    @State private var lastAnimatedWindow: HistoryWindow?
    @State private var hoveredSessionId: String?
    @State private var chartStyle: ChartStyle = .bars
    @State private var activityWindow: ActivityWindow = .days30

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            Divider()
            content
            // Active sessions temporarily hidden while we iterate on the
            // ledger-backed panels below — flip back to true to restore.
            if false, !store.activeSessions.isEmpty {
                Divider()
                activeSessionsSection
            }
            // Keep the section mounted whenever the ledger has *any* data
            // (current window or PR history) so the range picker stays
            // accessible even when the chosen window happens to be empty.
            if !store.activityCategories.isEmpty || !store.recentPRs.isEmpty {
                Divider()
                activityCategoriesSection
            }
            if !store.recentPRs.isEmpty {
                Divider()
                recentPRsSection
            }
            if let stats = store.sessionCostStats {
                Divider()
                sessionSpendDistributionSection(stats: stats)
            }
            if let stats = store.prCostStats {
                Divider()
                prSpendDistributionSection(stats: stats)
            }
            Divider()
            historySection
            if shouldShowLedgerEmptyState {
                Divider()
                ledgerEmptyState
            }
            Divider()
            footer
        }
        .padding(12)
        .frame(width: 280)
    }

    private var shouldShowLedgerEmptyState: Bool {
        store.activityCategories.isEmpty && store.recentPRs.isEmpty &&
            (store.ledgerStatus == .absent ||
             (store.ledgerStatus == .unknown ? false : isAvailableButEmpty))
    }

    private var isAvailableButEmpty: Bool {
        if case .available = store.ledgerStatus { return true }
        return false
    }

    @ViewBuilder
    private var ledgerEmptyState: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Activity & PR cost")
                .font(.subheadline.bold())
            switch store.ledgerStatus {
            case .absent, .unknown:
                Text("Install cc-ledger to see activity categories and per-PR cost.")
                    .font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Text("curl -fsSL https://ccledger.dev/install | bash")
                    .font(.caption2.monospaced())
                    .foregroundStyle(.secondary)
                    .textSelection(.enabled)
            case .versionMismatch(let actual, let expected):
                Text("Ledger DB schema v\(actual) is newer than menubar supports (v\(expected)). Update the menubar app.")
                    .font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            case .readError(let msg):
                Text("Couldn't read ledger.db: \(msg)")
                    .font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            case .available:
                Text("No activity yet — run a Claude Code session.")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private var activityCategoriesSection: some View {
        // Total across the visible top 5 — percentages are of "what we're
        // showing", which is the more honest framing than total-across-all.
        let top5 = Array(store.activityCategories.prefix(5))
        let total = top5.reduce(0.0) { $0 + $1.costUSD }
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Text("Activity").font(.subheadline.bold())
                Spacer()
                if !top5.isEmpty {
                    Text(String(format: "top 5 · $%.2f", total))
                        .font(.caption2).foregroundStyle(.secondary)
                }
                Picker("", selection: Binding(
                    get: { activityWindow },
                    set: { newValue in
                        activityWindow = newValue
                        Task { await store.setActivityWindowHours(newValue.sinceHours) }
                    }
                )) {
                    ForEach(ActivityWindow.allCases) { w in
                        Text(w.rawValue).tag(w)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .controlSize(.mini)
                .fixedSize()
                .tint(.claudeOrange)
            }
            if top5.isEmpty {
                Text("No activity in this window.")
                    .font(.caption).foregroundStyle(.secondary)
            } else {
                ForEach(top5) { cat in
                    ActivityCategoryRow(category: cat, totalCost: total)
                }
            }
        }
    }

    @ViewBuilder
    private var recentPRsSection: some View {
        let runawayThreshold = store.prCostStats?.runawayThreshold ?? .infinity
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Recent PRs").font(.subheadline.bold())
                Spacer()
                Text("from cc-ledger").font(.caption2).foregroundStyle(.secondary)
            }
            ForEach(store.recentPRs.prefix(4)) { pr in
                PRCostRow(
                    pr: pr,
                    isRunaway: pr.totalCostUSD > runawayThreshold,
                    hoveredPRId: $hoveredSessionId,   // reuse the same single-popover gate
                    store: store
                )
            }
        }
    }

    @ViewBuilder
    private func sessionSpendDistributionSection(stats: LedgerSessionCostStats) -> some View {
        // Runaway count is precomputed full-DB in LedgerStore — sessions don't
        // have a recent-list panel to filter against.
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text("Session spend").font(.subheadline.bold())
                Spacer()
                Text("\(stats.count) sessions")
                    .font(.caption2).foregroundStyle(.secondary)
            }
            Text(String(format: "p50 $%.2f · p95 $%.2f · p99 $%.2f",
                        stats.p50, stats.p95, stats.p99))
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
            if stats.runawayCount > 0 {
                HStack(spacing: 4) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.caption2)
                        .foregroundStyle(.orange)
                    Text("\(stats.runawayCount) runaway session\(stats.runawayCount == 1 ? "" : "s") above p95 (\(String(format: "$%.2f", stats.runawayThreshold)))")
                        .font(.caption2).foregroundStyle(.secondary)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    @ViewBuilder
    private func prSpendDistributionSection(stats: LedgerPRCostStats) -> some View {
        // Count runaways across the full DB, not just the displayed top-10
        // window — runaway detection is "this PR is unusual for our spend
        // history," not "this PR is unusual among recently-active PRs."
        let runawayCount = store.recentPRs.filter { $0.totalCostUSD > stats.runawayThreshold }.count
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text("PR spend").font(.subheadline.bold())
                Spacer()
                Text("\(stats.count) PRs")
                    .font(.caption2).foregroundStyle(.secondary)
            }
            Text(String(format: "p50 $%.2f · p95 $%.2f · p99 $%.2f",
                        stats.p50, stats.p95, stats.p99))
                .font(.caption.monospaced())
                .foregroundStyle(.secondary)
            if runawayCount > 0 {
                HStack(spacing: 4) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .font(.caption2)
                        .foregroundStyle(.orange)
                    Text("\(runawayCount) runaway PR\(runawayCount == 1 ? "" : "s") above p95 (\(String(format: "$%.2f", stats.runawayThreshold)))")
                        .font(.caption2).foregroundStyle(.secondary)
                        .lineLimit(2)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    @ViewBuilder
    private var activeSessionsSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Active sessions").font(.subheadline.bold())
                Spacer()
                Text("last hour").font(.caption2).foregroundStyle(.secondary)
            }
            ForEach(store.activeSessions.prefix(5)) { session in
                ActiveSessionRow(session: session, hoveredSessionId: $hoveredSessionId)
            }
            if store.activeSessions.count > 5 {
                Text("+ \(store.activeSessions.count - 5) more")
                    .font(.caption2).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder
    private var historySection: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack(spacing: 6) {
                Text("Usage").font(.subheadline.bold())
                Spacer()
                // Window picker is always mounted — just hidden in heatmap
                // mode via opacity so swapping modes never re-builds the view
                // tree. View-tree swaps were the source of the toggle jitter.
                Picker("", selection: Binding(
                    get: { window },
                    set: { newValue in
                        selectedDayKey = nil
                        withAnimation(.easeInOut(duration: 0.18)) {
                            window = newValue
                        }
                    }
                )) {
                    ForEach(HistoryWindow.allCases) { w in
                        Text(w.rawValue).tag(w)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .controlSize(.mini)
                .frame(width: 90)
                .tint(.claudeOrange)
                .opacity(chartStyle == .bars ? 1 : 0)
                .allowsHitTesting(chartStyle == .bars)
                Picker("", selection: Binding(
                    get: { chartStyle },
                    set: { newValue in
                        selectedDayKey = nil
                        chartStyle = newValue
                    }
                )) {
                    ForEach(ChartStyle.allCases) { s in
                        Image(systemName: s.icon).tag(s)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .controlSize(.mini)
                .fixedSize()
                .tint(.claudeOrange)
            }

            // Bars and heatmap are kept at the same chart-area height (80 pt)
            // so the section never resizes when the user toggles between them.
            let bars: [CostBar] = (chartStyle == .bars) ? currentBars() : heatmapBars()
            let isReady: Bool   = (chartStyle == .bars) ? storeReady : (store.history != nil)
            let isEmpty: Bool   = (chartStyle == .bars) && isReady && bars.isEmpty

            Group {
                if chartStyle == .bars {
                    headlineRow(bars: bars)
                } else {
                    heatmapHeadlineRow(bars: bars)
                }
            }
            .frame(height: 28, alignment: .topLeading)

            ZStack(alignment: .topLeading) {
                if !isReady {
                    HStack(spacing: 6) {
                        ProgressView().controlSize(.small)
                        Text("Scanning sessions…").font(.caption).foregroundStyle(.secondary)
                    }
                } else if isEmpty {
                    Text("No usage in this window.")
                        .font(.caption).foregroundStyle(.secondary)
                } else if chartStyle == .bars {
                    chartView(bars: bars)
                } else {
                    heatmapView(days: store.history?.days ?? [])
                }
            }
            .frame(maxWidth: .infinity, minHeight: 80, maxHeight: 80, alignment: .topLeading)
            // No animated transition when switching chart styles — the swap is
            // instant, no fade/slide.
            .animation(nil, value: chartStyle)

            detailRow(bars: bars)
        }
        .animation(nil, value: chartStyle)
    }

    private var storeReady: Bool {
        if window.isHourly { return store.hourlyHistory != nil }
        return store.history != nil
    }

    private func currentBars() -> [CostBar] {
        if window.isHourly {
            let all = store.hourlyHistory?.hours ?? []
            return Array(all.suffix(window.bucketCount)).map { h in
                CostBar(
                    key: h.hourKey,
                    displayLabel: formatHourLabel(h.hourStart),
                    cost: h.totalCostUSD,
                    totalTokens: h.totalTokens,
                    perModel: h.perModel
                )
            }
        } else {
            let all = store.history?.days ?? []
            return Array(all.suffix(window.bucketCount)).map { d in
                CostBar(
                    key: d.dayKey,
                    displayLabel: formatDayLabel(d.dayKey),
                    cost: d.totalCostUSD,
                    totalTokens: d.totalTokens,
                    perModel: d.perModel
                )
            }
        }
    }

    private var header: some View {
        HStack {
            VStack(alignment: .leading, spacing: 2) {
                Text("Claude Usage").font(.headline)
                if let plan = store.plan {
                    Text(plan.displayName).font(.caption).foregroundStyle(.secondary)
                }
            }
            Spacer()
            if store.isLoading {
                ProgressView().controlSize(.small)
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        if let err = store.errorMessage {
            VStack(alignment: .leading, spacing: 4) {
                Text("Error").font(.subheadline.bold()).foregroundStyle(.red)
                Text(err).font(.caption).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
        } else if let u = store.usage {
            VStack(alignment: .leading, spacing: 10) {
                fiveHourBurnChart(u.fiveHour)
                // Hide 7-day total when there's plenty of headroom (>20% left).
                if let sevenDay = u.sevenDay,
                   let pct = sevenDay.utilization, pct >= 80 {
                    window("7-day total", sevenDay)
                }
                if u.sevenDayOpus != nil   { window("7-day Opus",   u.sevenDayOpus) }
                // Hide Sonnet when there's plenty of headroom (>30% left).
                if let sonnet = u.sevenDaySonnet,
                   let pct = sonnet.utilization, pct >= 70 {
                    window("7-day Sonnet", sonnet)
                }
                // Hide extra usage when it's negligible (under $5 spent). API
                // returns values in cents (matching CodexBar's parseOverageSpendLimit
                // which divides by 100), so convert to dollars for display.
                if let extra = u.extraUsage, extra.isEnabled == true,
                   let usedCents = extra.usedCredits, let limitCents = extra.monthlyLimit {
                    let used = usedCents / 100
                    let limit = limitCents / 100
                    if used >= 5 {
                        Text(String(format: "Extra usage: $%.2f / $%.2f", used, limit))
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
            }
        } else {
            Text("Loading…").foregroundStyle(.secondary)
        }
    }

    private var footer: some View {
        HStack {
            Button("Refresh") { Task { await store.refresh() } }
            Spacer()
            if let updated = store.lastUpdated {
                Text(updated, style: .time).font(.caption2).foregroundStyle(.secondary)
            }
            Button("Quit") { NSApplication.shared.terminate(nil) }
        }
    }

    @ViewBuilder
    private func headlineRow(bars: [CostBar]) -> some View {
        let totalCost   = bars.reduce(0) { $0 + $1.cost }
        let totalTokens = bars.reduce(0) { $0 + $1.totalTokens }

        VStack(alignment: .leading, spacing: 0) {
            // Today goal line — always shown so the layout doesn't jump between windows.
            if let today = store.history?.today {
                let pct = min(100, Int(Double(today.totalTokens) / Double(dailyGoalTokens) * 100))
                let goalLabel = formatCompactTokens(dailyGoalTokens)
                Text(String(format: "Today: $%.2f · %@ · %d%% of %@",
                            today.totalCostUSD,
                            formatTokens(today.totalTokens),
                            pct,
                            goalLabel))
                    .font(.caption)
            }
            Text(String(format: "%@: $%.2f · %@",
                        window.headlineLabel, totalCost, formatTokens(totalTokens)))
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private func chartView(bars: [CostBar]) -> some View {
        let maxCost = max(bars.map(\.cost).max() ?? 0, 0.0001)
        let avgCost: Double = bars.isEmpty
            ? 0
            : bars.reduce(0) { $0 + $1.cost } / Double(bars.count)
        Chart {
            ForEach(bars) { bar in
                BarMark(
                    x: .value("Bucket", bar.key),
                    y: .value("Usage", bar.cost * growProgress)
                )
                .foregroundStyle(Color.claudeOrange)
            }
            if avgCost > 0 {
                RuleMark(y: .value("Average", avgCost * growProgress))
                    .foregroundStyle(Color.secondary.opacity(0.7))
                    .lineStyle(StrokeStyle(lineWidth: 1, dash: [3, 3]))
            }
        }
        .chartYScale(domain: 0...maxCost)
        .chartXAxis(.hidden)
        .chartYAxis(.hidden)
        .chartXSelection(value: $selectedDayKey)
        .chartBackground { proxy in
            GeometryReader { geo in
                if let rect = selectionBand(bars: bars, proxy: proxy, geo: geo) {
                    Rectangle()
                        .fill(Color.primary.opacity(0.12))
                        .frame(width: rect.width, height: rect.height)
                        .position(x: rect.midX, y: rect.midY)
                        .allowsHitTesting(false)
                }
            }
        }
        .frame(maxHeight: .infinity)
        .animation(nil, value: selectedDayKey)
        .task(id: window) {
            // Only animate on first appear or when the window actually changes —
            // not when the chart re-enters the tree from a heatmap→bars toggle.
            guard lastAnimatedWindow != window else { return }
            growProgress = 0
            try? await Task.sleep(for: .milliseconds(20))
            withAnimation(.easeOut(duration: 0.35)) {
                growProgress = 1
            }
            lastAnimatedWindow = window
        }
    }

    private func selectionBand(
        bars: [CostBar],
        proxy: ChartProxy,
        geo: GeometryProxy
    ) -> CGRect? {
        guard let key = selectedDayKey,
              let plotAnchor = proxy.plotFrame else { return nil }
        let plotFrame = geo[plotAnchor]
        guard !bars.isEmpty else { return nil }
        guard let centerX = proxy.position(forX: key) else { return nil }
        let bandWidth = plotFrame.width / CGFloat(bars.count)
        let x = plotFrame.origin.x + centerX
        return CGRect(
            x: x - bandWidth / 2,
            y: plotFrame.origin.y,
            width: bandWidth,
            height: plotFrame.height
        )
    }

    // MARK: - Heatmap

    /// Days inside the current heatmap window, mapped to CostBar so the same
    /// `headlineRow`/`detailRow` plumbing works without a parallel data type.
    private func heatmapBars() -> [CostBar] {
        let all = store.history?.days ?? []
        let cal = Calendar(identifier: .gregorian)
        let cutoff = cal.startOfDay(for: cal.date(byAdding: .day,
            value: -HeatmapConfig.weeks * 7 + 1, to: Date()) ?? Date())
        let f = DateFormatter(); f.dateFormat = "yyyy-MM-dd"
        return all.compactMap { d in
            guard let date = f.date(from: d.dayKey), date >= cutoff else { return nil }
            return CostBar(
                key: d.dayKey,
                displayLabel: formatDayLabel(d.dayKey),
                cost: d.totalCostUSD,
                totalTokens: d.totalTokens,
                perModel: d.perModel
            )
        }
    }

    @ViewBuilder
    private func heatmapHeadlineRow(bars: [CostBar]) -> some View {
        let totalCost   = bars.reduce(0) { $0 + $1.cost }
        let totalTokens = bars.reduce(0) { $0 + $1.totalTokens }
        VStack(alignment: .leading, spacing: 0) {
            if let today = store.history?.today {
                let pct = min(100, Int(Double(today.totalTokens) / Double(dailyGoalTokens) * 100))
                let goalLabel = formatCompactTokens(dailyGoalTokens)
                Text(String(format: "Today: $%.2f · %@ · %d%% of %@",
                            today.totalCostUSD,
                            formatTokens(today.totalTokens),
                            pct,
                            goalLabel))
                    .font(.caption)
            }
            Text(String(format: "%@: $%.2f · %@",
                        HeatmapConfig.headlineLabel, totalCost, formatTokens(totalTokens)))
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    /// 7-row × N-week grid (Sun..Sat top-to-bottom, oldest week leftmost).
    /// Cells are colored by daily total tokens; hovering a cell sets
    /// `selectedDayKey` so the shared `detailRow` updates.
    @ViewBuilder
    private func heatmapView(days: [DayUsage]) -> some View {
        let weeks = HeatmapConfig.weeks
        let side  = HeatmapConfig.cellSide
        let gap   = HeatmapConfig.cellGap

        let cal = Calendar(identifier: .gregorian)
        let today = cal.startOfDay(for: Date())
        // Walk back (weeks*7 - 1) days, then snap to the previous Sunday so the
        // grid has clean week columns. Calendar.gregorian uses Sunday=1.
        let span = max(0, weeks * 7 - 1)
        let rawStart = cal.date(byAdding: .day, value: -span, to: today) ?? today
        let weekday = cal.component(.weekday, from: rawStart)         // 1 = Sun
        let start = cal.date(byAdding: .day, value: -(weekday - 1), to: rawStart) ?? rawStart

        let dayFormatter: DateFormatter = {
            let f = DateFormatter()
            f.dateFormat = "yyyy-MM-dd"
            return f
        }()
        let monthFormatter: DateFormatter = {
            let f = DateFormatter()
            f.dateFormat = "MMM"
            return f
        }()
        let lookup = Dictionary(uniqueKeysWithValues: days.map { ($0.dayKey, $0) })
        let visibleMax = days.compactMap { d -> Int? in
            guard let date = dayFormatter.date(from: d.dayKey), date >= start, date <= today else { return nil }
            return d.totalTokens
        }.max() ?? 0

        // Month labels: show a month name above the first column whose Sunday
        // falls in a new month. Each label takes ~3 columns of width.
        let monthLabels: [(col: Int, label: String)] = (0..<weeks).compactMap { w in
            let colDate = cal.date(byAdding: .day, value: w * 7, to: start) ?? start
            let comps = cal.dateComponents([.month, .day], from: colDate)
            guard let day = comps.day, day <= 7 else { return nil }    // first week of month
            return (w, monthFormatter.string(from: colDate))
        }

        VStack(alignment: .leading, spacing: 2) {
            // Month strip
            ZStack(alignment: .topLeading) {
                Color.clear.frame(height: 10)
                ForEach(monthLabels, id: \.col) { entry in
                    Text(entry.label)
                        .font(.system(size: 8))
                        .foregroundStyle(.secondary)
                        .offset(x: 22 + CGFloat(entry.col) * (side + gap), y: 0)
                }
            }
            HStack(alignment: .top, spacing: 4) {
                // Day-of-week labels (Mon, Wed, Fri only)
                VStack(alignment: .trailing, spacing: gap) {
                    ForEach(0..<7) { row in
                        let label: String? = (row == 1) ? "Mon" : (row == 3) ? "Wed" : (row == 5) ? "Fri" : nil
                        Text(label ?? " ")
                            .font(.system(size: 7))
                            .foregroundStyle(.secondary)
                            .frame(width: 18, height: side, alignment: .trailing)
                            .opacity(label == nil ? 0 : 1)
                    }
                }
                // Cell grid
                HStack(alignment: .top, spacing: gap) {
                    ForEach(0..<weeks, id: \.self) { w in
                        VStack(spacing: gap) {
                            ForEach(0..<7, id: \.self) { row in
                                let date = cal.date(byAdding: .day, value: w * 7 + row, to: start) ?? start
                                let key = dayFormatter.string(from: date)
                                let inRange = date <= today && date >= start
                                let tokens = lookup[key]?.totalTokens ?? 0
                                let tier = heatmapTier(tokens: tokens, max: visibleMax)
                                RoundedRectangle(cornerRadius: 1, style: .continuous)
                                    .fill(heatmapColor(tier: tier))
                                    .frame(width: side, height: side)
                                    .opacity(inRange ? 1 : 0)
                                    .onHover { hovering in
                                        if hovering, inRange, lookup[key] != nil {
                                            selectedDayKey = key
                                        }
                                    }
                            }
                        }
                    }
                }
            }
        }
    }

    private func heatmapTier(tokens: Int, max: Int) -> Int {
        guard tokens > 0, max > 0 else { return 0 }
        let r = Double(tokens) / Double(max)
        if r < 0.25 { return 1 }
        if r < 0.50 { return 2 }
        if r < 0.75 { return 3 }
        return 4
    }

    private func heatmapColor(tier: Int) -> Color {
        switch tier {
        case 0:  return Color.primary.opacity(0.08)
        case 1:  return Color.claudeOrange.opacity(0.30)
        case 2:  return Color.claudeOrange.opacity(0.55)
        case 3:  return Color.claudeOrange.opacity(0.80)
        default: return Color.claudeOrange
        }
    }

    @ViewBuilder
    private func detailRow(bars: [CostBar]) -> some View {
        let bar: CostBar? = selectedDayKey.flatMap { key in bars.first { $0.key == key } }
        VStack(alignment: .leading, spacing: 0) {
            Text(detailPrimary(bar: bar))
                .font(.caption2)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .frame(height: 14, alignment: .leading)
            Text(detailSecondary(bar: bar) ?? " ")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .lineLimit(1)
                .frame(height: 14, alignment: .leading)
                .opacity(detailSecondary(bar: bar) == nil ? 0 : 1)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func detailPrimary(bar: CostBar?) -> String {
        guard let bar else { return "Hover a bar for details" }
        return String(format: "%@: $%.2f · %@", bar.displayLabel, bar.cost, formatTokens(bar.totalTokens))
    }

    private func detailSecondary(bar: CostBar?) -> String? {
        guard let bar else { return nil }
        let parts = bar.perModel
            .compactMap { (raw, m) -> (String, Double)? in
                guard m.costUSD > 0, let label = shortModel(raw) else { return nil }
                return (label, m.costUSD)
            }
            .sorted { $0.1 > $1.1 }
            .prefix(3)
            .map { (label, cost) in String(format: "%@ $%.2f", label, cost) }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    private func shortModel(_ model: String) -> String? {
        let lower = model.lowercased()
        if lower.contains("opus")   { return "Opus"   }
        if lower.contains("sonnet") { return "Sonnet" }
        if lower.contains("haiku")  { return "Haiku"  }
        return nil
    }

    private func formatDayLabel(_ key: String) -> String {
        let f = DateFormatter()
        f.dateFormat = "yyyy-MM-dd"
        guard let date = f.date(from: key) else { return key }
        let out = DateFormatter()
        out.dateFormat = "MMM d"
        return out.string(from: date)
    }

    private func formatHourLabel(_ date: Date) -> String {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")  // lock to "10 AM" / "2 PM"
        f.dateFormat = "h a"
        return f.string(from: date)
    }

    @ViewBuilder
    private func fiveHourBurnChart(_ w: ClaudeUsageWindow?) -> some View {
        if let w, let resetsAt = w.resetsAtDate, let pct = w.utilization {
            let sessionStart = resetsAt.addingTimeInterval(-5 * 3600)
            let now = Date()
            let total = resetsAt.timeIntervalSince(sessionStart)
            let elapsed = max(0, min(total, now.timeIntervalSince(sessionStart)))
            let onPace = elapsed / total * 100
            let delta = pct - onPace                  // + = burning faster than pace
            let samples = store.fiveHourSamples.filter { $0.timestamp >= sessionStart }

            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text("5-hour session").font(.caption)
                    Spacer()
                    Text(burnVerdict(delta: delta))
                        .font(.caption.monospaced())
                        .foregroundStyle(burnColor(delta: delta))
                }

                Chart {
                    // Target pace — dashed diagonal from (start, 0) to (reset, 100)
                    LineMark(
                        x: .value("t", sessionStart),
                        y: .value("%", 0),
                        series: .value("s", "target")
                    )
                    .foregroundStyle(.gray.opacity(0.5))
                    .lineStyle(StrokeStyle(lineWidth: 1, dash: [3, 3]))
                    LineMark(
                        x: .value("t", resetsAt),
                        y: .value("%", 100),
                        series: .value("s", "target")
                    )
                    .foregroundStyle(.gray.opacity(0.5))
                    .lineStyle(StrokeStyle(lineWidth: 1, dash: [3, 3]))

                    // 3. Used — filled area gradient + line on top
                    ForEach(samples, id: \.timestamp) { s in
                        AreaMark(
                            x: .value("t", s.timestamp),
                            y: .value("%", s.utilization),
                            series: .value("s", "used-area")
                        )
                        .foregroundStyle(
                            .linearGradient(
                                colors: [
                                    Color.claudeOrange.opacity(0.35),
                                    Color.claudeOrange.opacity(0)
                                ],
                                startPoint: .top,
                                endPoint: .bottom
                            )
                        )
                        .interpolationMethod(.linear)
                    }
                    ForEach(samples, id: \.timestamp) { s in
                        LineMark(
                            x: .value("t", s.timestamp),
                            y: .value("%", s.utilization),
                            series: .value("s", "used-line")
                        )
                        .foregroundStyle(Color.claudeOrange)
                        .lineStyle(StrokeStyle(lineWidth: 1.5))
                        .interpolationMethod(.linear)
                    }

                    // 4. "Now" vertical indicator
                    RuleMark(x: .value("t", now))
                        .foregroundStyle(.gray.opacity(0.3))
                        .lineStyle(StrokeStyle(lineWidth: 1))

                    // 5. Current point dot at the tip of the used line
                    PointMark(
                        x: .value("t", now),
                        y: .value("%", pct)
                    )
                    .foregroundStyle(Color.claudeOrange)
                    .symbolSize(35)
                }
                .chartXScale(domain: sessionStart...resetsAt)
                .chartYScale(domain: 0...100)
                .chartXAxis(.hidden)
                .chartYAxis(.hidden)
                .frame(height: 80)

                HStack {
                    Text(String(format: "%.0f%% used · on pace %.0f%%", pct, onPace))
                    Spacer()
                    Text("resets \(resetsAt, style: .relative)")
                }
                .font(.caption2).foregroundStyle(.secondary)
            }
        }
    }

    private func burnVerdict(delta: Double) -> String {
        if delta > 5  { return String(format: "+%.0f%% fast", delta) }
        if delta < -5 { return String(format: "%.0f%% slow",  delta) }
        return "on pace"
    }

    private func burnColor(delta: Double) -> Color {
        if delta > 5  { return .red }
        if delta < -5 { return Color(red: 0.30, green: 0.55, blue: 0.35) }
        return .secondary
    }

    @ViewBuilder
    private func window(_ label: String, _ w: ClaudeUsageWindow?) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(label).font(.caption)
                Spacer()
                Text(w?.utilization.map { String(format: "%.0f%% left", max(0, 100 - $0)) } ?? "—")
                    .font(.caption.monospaced())
            }
            ProgressView(value: max(0, min(1, (100 - (w?.utilization ?? 0)) / 100)))
                .tint(.claudeOrange)
            if let resets = w?.resetsAtDate {
                Text("resets \(resets, style: .relative)")
                    .font(.caption2).foregroundStyle(.secondary)
            }
        }
    }
}

func formatTokens(_ n: Int) -> String {
    if n >= 1_000_000_000 { return String(format: "%.1fB tokens", Double(n) / 1_000_000_000) }
    if n >= 1_000_000     { return String(format: "%.1fM tokens", Double(n) / 1_000_000) }
    if n >= 1_000         { return String(format: "%.1fK tokens", Double(n) / 1_000) }
    return "\(n) tokens"
}

struct ActiveSessionRow: View {
    let session: ActiveSession
    @Binding var hoveredSessionId: String?
    @State private var clearTask: Task<Void, Never>?

    private var isHovered: Bool { hoveredSessionId == session.id }

    var body: some View {
        HStack(spacing: 6) {
            if session.isSubagent {
                Image(systemName: "arrow.turn.down.right")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            Text(session.projectName)
                .font(.caption.weight(.semibold))
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer()
            Text("\(session.turnCount) turns")
                .font(.caption2.monospaced())
                .foregroundStyle(.secondary)
            Text(String(format: "$%.2f", session.costUSD))
                .font(.caption.monospaced())
                .foregroundStyle(Color.claudeOrange)
        }
        .contentShape(Rectangle())
        .padding(.horizontal, 4)
        .padding(.vertical, 2)
        .background(
            RoundedRectangle(cornerRadius: 4)
                .fill(isHovered ? Color.primary.opacity(0.06) : .clear)
        )
        .popover(isPresented: Binding(
            get: { isHovered },
            set: { if !$0 && isHovered { hoveredSessionId = nil } }
        ), arrowEdge: .leading) {
            ActiveSessionDetail(session: session)
                .padding(10)
                .frame(width: 260, alignment: .topLeading)
        }
        .onHover { hovering in
            clearTask?.cancel()
            if hovering {
                if hoveredSessionId == nil {
                    hoveredSessionId = session.id
                } else if hoveredSessionId != session.id {
                    // Another row's popover is open. Close it first, then open
                    // ours after it has time to fully dismiss — opening a new
                    // NSPopover while another is mid-close drops the new one.
                    let myId = session.id
                    hoveredSessionId = nil
                    clearTask = Task {
                        try? await Task.sleep(for: .milliseconds(180))
                        if Task.isCancelled { return }
                        await MainActor.run { hoveredSessionId = myId }
                    }
                }
            } else if hoveredSessionId == session.id {
                let myId = session.id
                clearTask = Task {
                    try? await Task.sleep(for: .milliseconds(120))
                    if Task.isCancelled { return }
                    await MainActor.run {
                        if hoveredSessionId == myId {
                            hoveredSessionId = nil
                        }
                    }
                }
            }
        }
    }
}

struct ActiveSessionDetail: View {
    let session: ActiveSession

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text("\(session.projectName) · \(relative(session.lastActivityAt)) · started \(relative(session.firstActivityAt))")
                .font(.caption2).foregroundStyle(.secondary)
                .lineLimit(1)

            // Per-model breakdown (only models with non-zero cost)
            let models = session.perModel
                .compactMap { (raw, m) -> (String, Double)? in
                    guard m.costUSD > 0, let label = ActiveSessionDetail.shortModel(raw) else { return nil }
                    return (label, m.costUSD)
                }
                .sorted { $0.1 > $1.1 }
            if !models.isEmpty {
                HStack(spacing: 8) {
                    ForEach(models, id: \.0) { label, cost in
                        Text("\(label) $\(String(format: "%.2f", cost))")
                            .font(.caption2.monospaced())
                            .foregroundStyle(.secondary)
                    }
                }
            }

            Text(formatTokens(session.totalTokens))
                .font(.caption2.monospaced())
                .foregroundStyle(.secondary)

            let tools = session.toolCounts
                .sorted { $0.value > $1.value }
                .prefix(5)
            if !tools.isEmpty {
                HStack(spacing: 6) {
                    Text("Tools:").font(.caption2).foregroundStyle(.secondary)
                    ForEach(Array(tools), id: \.key) { name, count in
                        Text("\(name) ×\(count)")
                            .font(.caption2.monospaced())
                            .foregroundStyle(.secondary)
                    }
                }
                .lineLimit(1)
            }
        }
    }

    private func relative(_ date: Date) -> String {
        let f = RelativeDateTimeFormatter()
        f.unitsStyle = .abbreviated
        return f.localizedString(for: date, relativeTo: Date())
    }

    static func shortModel(_ model: String) -> String? {
        let lower = model.lowercased()
        if lower.contains("opus")   { return "Opus"   }
        if lower.contains("sonnet") { return "Sonnet" }
        if lower.contains("haiku")  { return "Haiku"  }
        return nil
    }
}

struct ActivityCategoryRow: View {
    let category: LedgerActivityCategory
    let totalCost: Double

    private var fraction: Double {
        guard totalCost > 0 else { return 0 }
        return min(1, category.costUSD / totalCost)
    }

    var body: some View {
        HStack(spacing: 6) {
            Text(category.displayName)
                .font(.caption.weight(.semibold))
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 6)
            // Thin proportion bar.
            GeometryReader { geo in
                ZStack(alignment: .leading) {
                    Capsule().fill(Color.primary.opacity(0.08))
                    Capsule().fill(Color.claudeOrange.opacity(0.85))
                        .frame(width: geo.size.width * fraction)
                }
            }
            .frame(width: 60, height: 4)
            Text(String(format: "$%.2f", category.costUSD))
                .font(.caption.monospaced())
                .foregroundStyle(Color.claudeOrange)
                .frame(width: 56, alignment: .trailing)
        }
        .padding(.horizontal, 4)
        .padding(.vertical, 2)
    }
}

struct PRCostRow: View {
    let pr: LedgerPRCost
    var isRunaway: Bool = false
    @Binding var hoveredPRId: String?
    let store: UsageStore
    @State private var clearTask: Task<Void, Never>?
    @State private var breakdown: [LedgerActivityCategory]?

    private var isHovered: Bool { hoveredPRId == pr.id }

    private var stateColor: Color {
        switch pr.state {
        case "merged": return .purple
        case "closed": return .red
        default:       return .green   // open
        }
    }

    var body: some View {
        HStack(spacing: 6) {
            Circle()
                .fill(stateColor.opacity(0.8))
                .frame(width: 6, height: 6)
            Text("\(pr.repoShortName) #\(pr.prNumber)")
                .font(.caption.weight(.semibold))
                .lineLimit(1)
                .truncationMode(.middle)
            if isRunaway {
                Image(systemName: "exclamationmark.triangle.fill")
                    .font(.caption2)
                    .foregroundStyle(.orange)
                    .help("Cost is above the p95 of all PRs on file.")
            }
            Spacer(minLength: 6)
            Text("\(pr.aiLinesAdded) AI lines")
                .font(.caption2.monospaced())
                .foregroundStyle(.secondary)
                .lineLimit(1)
            Text(String(format: "$%.2f", pr.totalCostUSD))
                .font(.caption.monospaced())
                .foregroundStyle(isRunaway ? .orange : Color.claudeOrange)
                .frame(width: 56, alignment: .trailing)
        }
        .contentShape(Rectangle())
        .padding(.horizontal, 4)
        .padding(.vertical, 2)
        .background(
            RoundedRectangle(cornerRadius: 4)
                .fill(isHovered ? Color.primary.opacity(0.06) : .clear)
        )
        .popover(isPresented: Binding(
            get: { isHovered },
            set: { if !$0 && isHovered { hoveredPRId = nil } }
        ), arrowEdge: .leading) {
            PRBreakdownDetail(pr: pr, breakdown: breakdown ?? [])
                .padding(10)
                .frame(width: 260, alignment: .topLeading)
        }
        .onHover { hovering in
            clearTask?.cancel()
            if hovering {
                if hoveredPRId == nil {
                    hoveredPRId = pr.id
                    Task { breakdown = await store.categoryBreakdownForPR(cwd: pr.cwd, prNumber: pr.prNumber) }
                } else if hoveredPRId != pr.id {
                    // Another popover is open — close it first, then ours
                    // after enough time for the dismiss to complete.
                    let myId = pr.id
                    let myCwd = pr.cwd
                    let myNum = pr.prNumber
                    hoveredPRId = nil
                    breakdown = nil
                    clearTask = Task {
                        try? await Task.sleep(for: .milliseconds(180))
                        if Task.isCancelled { return }
                        await MainActor.run { hoveredPRId = myId }
                        breakdown = await store.categoryBreakdownForPR(cwd: myCwd, prNumber: myNum)
                    }
                }
            } else if hoveredPRId == pr.id {
                let myId = pr.id
                clearTask = Task {
                    try? await Task.sleep(for: .milliseconds(120))
                    if Task.isCancelled { return }
                    await MainActor.run {
                        if hoveredPRId == myId {
                            hoveredPRId = nil
                            breakdown = nil
                        }
                    }
                }
            }
        }
    }
}

struct PRBreakdownDetail: View {
    let pr: LedgerPRCost
    let breakdown: [LedgerActivityCategory]

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("\(pr.repoShortName) #\(pr.prNumber) · \(pr.branch)")
                .font(.caption2.bold())
                .lineLimit(1)
                .truncationMode(.middle)
            Text("\(pr.state) · \(pr.commitCount) commits · \(pr.sessionCount) sessions · \(pr.aiLinesAdded) AI lines")
                .font(.caption2)
                .foregroundStyle(.secondary)
            if breakdown.isEmpty {
                Text("Loading category mix…")
                    .font(.caption2).foregroundStyle(.secondary)
                    .padding(.top, 4)
            } else {
                let total = breakdown.reduce(0.0) { $0 + $1.costUSD }
                Divider().padding(.vertical, 2)
                Text("Activity in this PR")
                    .font(.caption2.bold())
                ForEach(breakdown.prefix(6)) { cat in
                    let pct = total > 0 ? cat.costUSD / total * 100 : 0
                    HStack(spacing: 6) {
                        Text(cat.displayName)
                            .font(.caption2)
                            .lineLimit(1)
                        Spacer(minLength: 6)
                        GeometryReader { geo in
                            ZStack(alignment: .leading) {
                                Capsule().fill(Color.primary.opacity(0.08))
                                Capsule().fill(Color.claudeOrange.opacity(0.85))
                                    .frame(width: geo.size.width * min(1, pct / 100))
                            }
                        }
                        .frame(width: 50, height: 4)
                        Text(String(format: "%.0f%%", pct))
                            .font(.caption2.monospaced())
                            .foregroundStyle(.secondary)
                            .frame(width: 32, alignment: .trailing)
                    }
                }
            }
        }
    }
}
