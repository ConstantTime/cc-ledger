import Foundation
import MenuBarCore

@main
struct ClaudeUsageProbe {
    static let barWidth = 50

    static func main() async {
        do {
            let creds = try ClaudeCredentialsStore.load()
            print("✓ Loaded credentials from \(creds.source)")
            if let plan = creds.plan {
                print("  plan: \(plan.displayName)")
            }
            if let exp = creds.expiresAt {
                print("  expires: \(exp) (\(exp > Date() ? "valid" : "EXPIRED"))")
            }
            print("")

            let usage = try await ClaudeUsageClient().fetch(accessToken: creds.accessToken)

            printWindow("Current session",                 usage.fiveHour)
            printWindow("Current week (all models)",       usage.sevenDay)
            printWindow("Current week (Opus only)",        usage.sevenDayOpus)
            printWindow("Current week (Sonnet only)",      usage.sevenDaySonnet)

            if let extra = usage.extraUsage, extra.isEnabled == true,
               let used = extra.usedCredits, let limit = extra.monthlyLimit
            {
                let currency = extra.currency ?? "USD"
                print(String(format: "Extra usage: %.2f / %.2f %@", used, limit, currency))
            }
        } catch {
            FileHandle.standardError.write(Data("Error: \(error.localizedDescription)\n".utf8))
            exit(1)
        }
    }

    static func printWindow(_ label: String, _ w: ClaudeUsageWindow?) {
        guard let w, let util = w.utilization else { return }
        print(label)
        print("\(renderBar(util / 100))   \(Int(util.rounded()))% used")
        if let resets = w.resetsAtDate {
            print("Resets \(formatReset(resets))")
        }
        print("")
    }

    static func renderBar(_ ratio: Double) -> String {
        let clamped = max(0, min(1, ratio))
        let filled = Int((clamped * Double(barWidth)).rounded())
        return String(repeating: "█", count: filled)
            + String(repeating: "░", count: barWidth - filled)
    }

    static func formatReset(_ date: Date) -> String {
        let cal = Calendar.current
        let minute = cal.component(.minute, from: date)
        let timeFmt = DateFormatter()
        timeFmt.dateFormat = minute == 0 ? "ha" : "h:mma"
        timeFmt.amSymbol = "am"
        timeFmt.pmSymbol = "pm"
        let time = timeFmt.string(from: date)
        let tz = TimeZone.current.identifier

        if cal.isDateInToday(date) {
            return "\(time) (\(tz))"
        }
        let dateFmt = DateFormatter()
        dateFmt.dateFormat = "MMM d"
        return "\(dateFmt.string(from: date)) at \(time) (\(tz))"
    }
}
