import Foundation

/// Per-model Claude pricing — mirrors CodexBar's `CostUsagePricing.swift`
/// (`~/code/CodexBar/Sources/CodexBarCore/Vendored/CostUsage/CostUsagePricing.swift`)
/// which has been validated against Anthropic invoices in production.
///
/// We dropped the previous `ClaudeModelFamily` enum because substring matching
/// can't tell Opus 4.1 ($15/Mtok input) from Opus 4.5+ ($5/Mtok input).
public enum Pricing {
    /// Cost in USD for one turn given raw token counts. Returns 0 for
    /// unrecognized models so we don't fabricate numbers.
    public static func cost(
        model: String,
        inputTokens: Int,
        outputTokens: Int,
        cacheReadTokens: Int,
        cacheCreationTokens: Int
    ) -> Double {
        let key = normalize(model)
        guard let p = table[key] else { return 0 }

        return tiered(max(0, inputTokens),
                      base: p.input,
                      above: p.inputAbove,
                      threshold: p.thresholdTokens)
             + tiered(max(0, cacheReadTokens),
                      base: p.cacheRead,
                      above: p.cacheReadAbove,
                      threshold: p.thresholdTokens)
             + tiered(max(0, cacheCreationTokens),
                      base: p.cacheCreation,
                      above: p.cacheCreationAbove,
                      threshold: p.thresholdTokens)
             + tiered(max(0, outputTokens),
                      base: p.output,
                      above: p.outputAbove,
                      threshold: p.thresholdTokens)
    }

    // MARK: - rate table

    private static let table: [String: ClaudePricing] = [
        // Haiku 4.5 — $1 / $5 / $1.25 / $0.10 per Mtok
        "claude-haiku-4-5":          .haiku45,
        "claude-haiku-4-5-20251001": .haiku45,

        // Sonnet 4 / 4.5 / 4.6 — $3 / $15 / $3.75 / $0.30 base, doubles above 200K
        "claude-sonnet-4-5":          .sonnet4x,
        "claude-sonnet-4-5-20250929": .sonnet4x,
        "claude-sonnet-4-6":          .sonnet4x,
        "claude-sonnet-4-20250514":   .sonnet4x,

        // Opus 4.5 / 4.6 / 4.7 / 4.8 — $5 / $25 / $6.25 / $0.50 per Mtok (current)
        "claude-opus-4-5":          .opus45,
        "claude-opus-4-5-20251101": .opus45,
        "claude-opus-4-6":          .opus45,
        "claude-opus-4-6-20260205": .opus45,
        "claude-opus-4-7":          .opus45,
        "claude-opus-4-8":          .opus45,

        // Opus 4.0 / 4.1 (legacy) — $15 / $75 / $18.75 / $1.50 per Mtok
        "claude-opus-4-1":          .opus41,
        "claude-opus-4-20250514":   .opus41,
    ]

    // MARK: - helpers

    private static func tiered(
        _ tokens: Int, base: Double, above: Double?, threshold: Int?
    ) -> Double {
        guard let threshold, let above else { return Double(tokens) * base }
        let below = min(tokens, threshold)
        let over  = max(tokens - threshold, 0)
        return Double(below) * base + Double(over) * above
    }

    /// Strips Bedrock prefixes/suffixes and falls back to a base name (without
    /// dated suffix) if the dated variant isn't in the table.
    ///
    /// Examples:
    ///   "anthropic.claude-opus-4-7"        → "claude-opus-4-7"
    ///   "us.anthropic.claude-opus-4-7-v1:0" → "claude-opus-4-7"
    ///   "claude-opus-4-7-20260205"          → "claude-opus-4-7"
    private static func normalize(_ raw: String) -> String {
        var s = raw.trimmingCharacters(in: .whitespacesAndNewlines)

        if s.hasPrefix("anthropic.") {
            s = String(s.dropFirst("anthropic.".count))
        }

        // "us.anthropic.claude-opus-4-7-v1:0" — strip up to last "." if a
        // claude- model name follows.
        if let lastDot = s.lastIndex(of: "."), s.contains("claude-") {
            let tail = String(s[s.index(after: lastDot)...])
            if tail.hasPrefix("claude-") { s = tail }
        }

        // Bedrock version suffix: "-v1:0", "-v2:0", etc.
        if let v = s.range(of: #"-v\d+:\d+$"#, options: .regularExpression) {
            s.removeSubrange(v)
        }

        if table[s] != nil { return s }

        // Dated suffix: "-20260205", "-20251101", etc.
        if let dated = s.range(of: #"-\d{8}$"#, options: .regularExpression) {
            let base = String(s[..<dated.lowerBound])
            if table[base] != nil { return base }
        }

        return s
    }
}

// MARK: - rate struct

/// All pricing fields for one Claude model. Cache-creation = 1.25× input,
/// cache-read = 0.10× input, per Anthropic's pricing page. The `*Above`
/// fields apply when input context exceeds `thresholdTokens` (Sonnet only).
private struct ClaudePricing {
    let input: Double             // USD per token
    let output: Double
    let cacheCreation: Double
    let cacheRead: Double

    let thresholdTokens: Int?
    let inputAbove: Double?
    let outputAbove: Double?
    let cacheCreationAbove: Double?
    let cacheReadAbove: Double?

    static let haiku45 = ClaudePricing(
        input: 1e-6,             output: 5e-6,
        cacheCreation: 1.25e-6,  cacheRead: 1e-7,
        thresholdTokens: nil,
        inputAbove: nil,         outputAbove: nil,
        cacheCreationAbove: nil, cacheReadAbove: nil
    )

    static let sonnet4x = ClaudePricing(
        input: 3e-6,             output: 1.5e-5,
        cacheCreation: 3.75e-6,  cacheRead: 3e-7,
        thresholdTokens: 200_000,
        inputAbove: 6e-6,        outputAbove: 2.25e-5,
        cacheCreationAbove: 7.5e-6, cacheReadAbove: 6e-7
    )

    static let opus45 = ClaudePricing(
        input: 5e-6,             output: 2.5e-5,
        cacheCreation: 6.25e-6,  cacheRead: 5e-7,
        thresholdTokens: nil,
        inputAbove: nil,         outputAbove: nil,
        cacheCreationAbove: nil, cacheReadAbove: nil
    )

    static let opus41 = ClaudePricing(
        input: 1.5e-5,           output: 7.5e-5,
        cacheCreation: 1.875e-5, cacheRead: 1.5e-6,
        thresholdTokens: nil,
        inputAbove: nil,         outputAbove: nil,
        cacheCreationAbove: nil, cacheReadAbove: nil
    )
}
