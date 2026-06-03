import Foundation
#if canImport(Security)
import Security
#endif

public struct ClaudeCredentials: Sendable {
    public let accessToken: String
    public let refreshToken: String?
    public let expiresAt: Date?
    public let scopes: [String]
    public let rateLimitTier: String?
    public let source: Source

    public var plan: ClaudePlan? { ClaudePlan(rateLimitTier: rateLimitTier) }

    public enum Source: Sendable, CustomStringConvertible {
        case file(URL)
        case keychain

        public var description: String {
            switch self {
            case .file(let url): return url.path
            case .keychain:      return "macOS Keychain (Claude Code-credentials)"
            }
        }
    }
}

public enum ClaudeCredentialsError: Error, LocalizedError {
    case notFound(triedFile: URL, keychainStatus: Int32?)
    /// The blob isn't valid JSON at all — likely corruption.
    case unreadableJSON
    /// JSON parsed fine but has no `claudeAiOauth` key. Expected for users on
    /// API keys / Vertex / Bedrock — Claude Code still writes a credentials
    /// blob (e.g. for MCP OAuth tokens) but never populates `claudeAiOauth`.
    case noClaudeAiOauth
    case missingAccessToken

    public var errorDescription: String? {
        switch self {
        case .notFound(let url, let status):
            var msg = "No Claude credentials found.\n• File: \(url.path) (missing)"
            if let status {
                msg += "\n• Keychain: not found (OSStatus \(status))"
            }
            msg += "\nRun `claude` to log in."
            return msg
        case .unreadableJSON:
            return "Claude credentials blob is not valid JSON."
        case .noClaudeAiOauth:
            return "No Claude.ai OAuth token in credentials (API / Vertex / Bedrock mode)."
        case .missingAccessToken:
            return "claudeAiOauth.accessToken missing from credentials."
        }
    }

    /// True when there is no Claude.ai OAuth token on the machine — either
    /// no creds file, nothing usable in Keychain, or the credentials blob
    /// exists but has no `claudeAiOauth` key. This is the expected state
    /// for users on API keys, Vertex, or Bedrock — the subscription quota
    /// endpoint simply doesn't apply, so the UI should degrade to a local-
    /// ledger-only view rather than treating it as an error. Truly corrupt
    /// JSON is NOT included — that should still surface as an error.
    public var isNoSubscriptionAuth: Bool {
        switch self {
        case .notFound, .noClaudeAiOauth, .missingAccessToken: return true
        case .unreadableJSON:                                  return false
        }
    }
}

public enum ClaudeCredentialsStore {
    public static let keychainService = "Claude Code-credentials"
    public static let defaultPath = "~/.claude/.credentials.json"

    public static func load() throws -> ClaudeCredentials {
        let url = defaultURL()
        if FileManager.default.fileExists(atPath: url.path) {
            let data = try Data(contentsOf: url)
            return try parse(data: data, source: .file(url))
        }

        #if canImport(Security)
        let (data, status) = readKeychain()
        if let data {
            return try parse(data: data, source: .keychain)
        }
        throw ClaudeCredentialsError.notFound(triedFile: url, keychainStatus: status)
        #else
        throw ClaudeCredentialsError.notFound(triedFile: url, keychainStatus: nil)
        #endif
    }

    public static func defaultURL() -> URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".claude/.credentials.json")
    }

    private static func parse(data: Data, source: ClaudeCredentials.Source) throws -> ClaudeCredentials {
        let json: Any
        do {
            json = try JSONSerialization.jsonObject(with: data)
        } catch {
            throw ClaudeCredentialsError.unreadableJSON
        }
        guard let root = json as? [String: Any] else {
            throw ClaudeCredentialsError.unreadableJSON
        }
        guard let oauth = root["claudeAiOauth"] as? [String: Any] else {
            // JSON is well-formed but has no claudeAiOauth — typical for
            // API / Vertex / Bedrock users whose Keychain blob only carries
            // MCP OAuth tokens.
            throw ClaudeCredentialsError.noClaudeAiOauth
        }
        guard let accessToken = oauth["accessToken"] as? String, !accessToken.isEmpty else {
            throw ClaudeCredentialsError.missingAccessToken
        }
        let refreshToken = oauth["refreshToken"] as? String
        let expiresAt: Date? = (oauth["expiresAt"] as? Double).map {
            Date(timeIntervalSince1970: $0 / 1000.0)
        }
        let scopes = (oauth["scopes"] as? [String]) ?? []
        let rateLimitTier = oauth["rateLimitTier"] as? String
        return ClaudeCredentials(
            accessToken: accessToken,
            refreshToken: refreshToken,
            expiresAt: expiresAt,
            scopes: scopes,
            rateLimitTier: rateLimitTier,
            source: source
        )
    }

    #if canImport(Security)
    private static func readKeychain() -> (Data?, OSStatus) {
        let query: [String: Any] = [
            kSecClass as String:       kSecClassGenericPassword,
            kSecAttrService as String: keychainService,
            kSecReturnData as String:  true,
            kSecMatchLimit as String:  kSecMatchLimitOne,
        ]
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        guard status == errSecSuccess, let data = item as? Data else {
            return (nil, status)
        }
        return (data, status)
    }
    #endif
}
