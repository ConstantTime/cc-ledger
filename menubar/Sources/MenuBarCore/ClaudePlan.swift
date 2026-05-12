import Foundation

public enum ClaudePlan: String, CaseIterable, Sendable {
    case pro
    case max
    case team
    case enterprise
    case ultra

    public var displayName: String {
        switch self {
        case .pro:        return "Claude Pro"
        case .max:        return "Claude Max"
        case .team:       return "Claude Team"
        case .enterprise: return "Claude Enterprise"
        case .ultra:      return "Claude Ultra"
        }
    }

    public init?(rateLimitTier: String?) {
        let tier = (rateLimitTier ?? "")
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased()
        guard !tier.isEmpty else { return nil }
        if tier.contains("ultra")      { self = .ultra; return }
        if tier.contains("enterprise") { self = .enterprise; return }
        if tier.contains("team")       { self = .team; return }
        if tier.contains("max")        { self = .max; return }
        if tier.contains("pro")        { self = .pro; return }
        return nil
    }
}
