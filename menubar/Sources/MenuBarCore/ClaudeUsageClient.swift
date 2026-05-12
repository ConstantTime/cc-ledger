import Foundation

public enum ClaudeUsageError: Error, LocalizedError {
    case unauthorized
    case http(Int, String?)
    case invalidResponse
    case network(Error)

    public var errorDescription: String? {
        switch self {
        case .unauthorized:
            return "Unauthorized (401). Run `claude` to re-authenticate."
        case .http(let code, let body):
            return "HTTP \(code): \(body ?? "")"
        case .invalidResponse:
            return "Invalid response from Claude usage endpoint."
        case .network(let err):
            return "Network error: \(err.localizedDescription)"
        }
    }
}

public actor ClaudeUsageClient {
    public static let endpoint = URL(string: "https://api.anthropic.com/api/oauth/usage")!
    private static let betaHeader = "oauth-2025-04-20"
    private static let userAgent = "claude-code/2.1.0"

    public init() {}

    public func fetch(accessToken: String) async throws -> ClaudeUsageResponse {
        var request = URLRequest(url: Self.endpoint)
        request.httpMethod = "GET"
        request.timeoutInterval = 30
        request.setValue("Bearer \(accessToken)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Accept")
        request.setValue(Self.betaHeader, forHTTPHeaderField: "anthropic-beta")
        request.setValue(Self.userAgent, forHTTPHeaderField: "User-Agent")

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await URLSession.shared.data(for: request)
        } catch {
            throw ClaudeUsageError.network(error)
        }

        guard let http = response as? HTTPURLResponse else {
            throw ClaudeUsageError.invalidResponse
        }
        switch http.statusCode {
        case 200:
            do {
                return try JSONDecoder().decode(ClaudeUsageResponse.self, from: data)
            } catch {
                throw ClaudeUsageError.invalidResponse
            }
        case 401:
            throw ClaudeUsageError.unauthorized
        default:
            throw ClaudeUsageError.http(http.statusCode, String(data: data, encoding: .utf8))
        }
    }
}
