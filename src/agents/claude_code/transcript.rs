//! Pure: parse a Claude Code transcript JSONL into `Vec<TurnUsage>`.
//!
//! Every JSONL line is a top-level object with `type` and `timestamp`.
//! Assistant entries that carry `message.usage` become a [`TurnUsage`];
//! other entries (`user`, `system`, `summary`, …) are walked only to track
//! the previous-message timestamp (used as `started_at_ms` for the next
//! assistant turn). Malformed lines are silently skipped — that mirrors
//! how the agent itself recovers from corrupt transcript tails.

use serde_json::Value;

/// Per-turn data extracted from the transcript. Numeric fields default to 0
/// when missing so `Default` is enough for tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnUsage {
    pub turn_idx: i64,
    pub model: Option<String>,
    /// Timestamp of the previous (non-assistant) message in the transcript.
    pub started_at_ms: Option<i64>,
    /// Timestamp on the assistant message itself.
    pub ended_at_ms: Option<i64>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read: i64,
    pub cache_write_5m: i64,
    pub cache_write_1h: i64,
    pub service_tier: Option<String>,
    pub web_search_count: i64,
}

/// Parse the transcript bytes (decoded as lossy UTF-8 by the caller) into
/// one [`TurnUsage`] per assistant message that carries `usage`.
pub fn parse_usages(jsonl: &str) -> Vec<TurnUsage> {
    let mut prev_ts: Option<i64> = None;
    let mut idx: i64 = 0;
    let mut out = Vec::new();

    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let typ = v.get("type").and_then(Value::as_str).unwrap_or("");
        let ts_ms = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_iso_ms);

        match typ {
            "assistant" => {
                if let Some(usage) = v.get("message").and_then(|m| m.get("usage")) {
                    let model = v
                        .get("message")
                        .and_then(|m| m.get("model"))
                        .and_then(Value::as_str)
                        .map(String::from);

                    let mut u = TurnUsage {
                        turn_idx: idx,
                        model,
                        started_at_ms: prev_ts,
                        ended_at_ms: ts_ms,
                        input_tokens: as_i64(usage, "input_tokens"),
                        output_tokens: as_i64(usage, "output_tokens"),
                        cache_read: as_i64(usage, "cache_read_input_tokens"),
                        cache_write_5m: 0,
                        cache_write_1h: 0,
                        service_tier: usage
                            .get("service_tier")
                            .and_then(Value::as_str)
                            .map(String::from),
                        web_search_count: usage
                            .pointer("/server_tool_use/web_search_requests")
                            .and_then(Value::as_i64)
                            .unwrap_or(0),
                    };

                    // Cache write breakdown: prefer the explicit 5m/1h split
                    // when present; otherwise treat the lump-sum value as 5m
                    // (Anthropic's default TTL).
                    if let Some(cc) = usage.get("cache_creation") {
                        u.cache_write_5m = as_i64(cc, "ephemeral_5m_input_tokens");
                        u.cache_write_1h = as_i64(cc, "ephemeral_1h_input_tokens");
                    } else {
                        u.cache_write_5m = as_i64(usage, "cache_creation_input_tokens");
                    }

                    out.push(u);
                    idx += 1;
                }
                prev_ts = ts_ms.or(prev_ts);
            }
            "user" => {
                prev_ts = ts_ms.or(prev_ts);
            }
            _ => {
                // system / summary / unknown — don't touch prev_ts so an
                // assistant turn's started_at points at real user content.
            }
        }
    }
    out
}

fn as_i64(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(Value::as_i64).unwrap_or(0)
}

fn parse_iso_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_yields_no_turns() {
        assert!(parse_usages("").is_empty());
        assert!(parse_usages("\n\n").is_empty());
    }

    #[test]
    fn ignores_malformed_lines() {
        let jsonl = "{this is not json}\n{\"type\":\"user\"}\n";
        assert!(parse_usages(jsonl).is_empty());
    }

    #[test]
    fn single_assistant_with_usage_is_one_turn() {
        let jsonl = r#"{"type":"user","timestamp":"2026-05-02T10:00:00Z"}
{"type":"assistant","timestamp":"2026-05-02T10:00:01Z","message":{"model":"claude-opus-4-7-20260301","usage":{"input_tokens":100,"output_tokens":50,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"service_tier":"standard"}}}"#;
        let u = parse_usages(jsonl);
        assert_eq!(u.len(), 1);
        let t = &u[0];
        assert_eq!(t.turn_idx, 0);
        assert_eq!(t.model.as_deref(), Some("claude-opus-4-7-20260301"));
        assert_eq!(t.input_tokens, 100);
        assert_eq!(t.output_tokens, 50);
        assert_eq!(t.cache_read, 0);
        assert_eq!(t.cache_write_5m, 0);
        assert_eq!(t.cache_write_1h, 0);
        assert_eq!(t.service_tier.as_deref(), Some("standard"));
        assert!(t.started_at_ms.is_some());
        assert!(t.ended_at_ms.is_some());
        assert!(t.ended_at_ms > t.started_at_ms);
    }

    #[test]
    fn cache_write_falls_back_to_5m_without_breakdown() {
        let jsonl = r#"{"type":"assistant","message":{"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":1000}}}"#;
        let u = parse_usages(jsonl);
        assert_eq!(u[0].cache_write_5m, 1000);
        assert_eq!(u[0].cache_write_1h, 0);
    }

    #[test]
    fn cache_creation_object_splits_5m_vs_1h() {
        let jsonl = r#"{"type":"assistant","message":{"usage":{"input_tokens":0,"output_tokens":0,"cache_creation_input_tokens":5000,"cache_creation":{"ephemeral_5m_input_tokens":4000,"ephemeral_1h_input_tokens":1000}}}}"#;
        let u = parse_usages(jsonl);
        assert_eq!(u[0].cache_write_5m, 4000);
        assert_eq!(u[0].cache_write_1h, 1000);
    }

    #[test]
    fn web_search_count_extracted() {
        let jsonl = r#"{"type":"assistant","message":{"usage":{"input_tokens":0,"output_tokens":0,"server_tool_use":{"web_search_requests":3}}}}"#;
        let u = parse_usages(jsonl);
        assert_eq!(u[0].web_search_count, 3);
    }

    #[test]
    fn missing_usage_skips_assistant() {
        let jsonl = r#"{"type":"assistant","message":{"model":"claude-opus-4-7"}}"#;
        assert!(parse_usages(jsonl).is_empty());
    }

    #[test]
    fn turn_idx_increments_per_assistant_with_usage() {
        let jsonl = r#"{"type":"user","timestamp":"2026-05-02T10:00:00Z"}
{"type":"assistant","timestamp":"2026-05-02T10:00:01Z","message":{"usage":{"input_tokens":1,"output_tokens":1}}}
{"type":"user","timestamp":"2026-05-02T10:00:02Z"}
{"type":"assistant","timestamp":"2026-05-02T10:00:03Z","message":{"usage":{"input_tokens":2,"output_tokens":2}}}"#;
        let u = parse_usages(jsonl);
        assert_eq!(u.len(), 2);
        assert_eq!(u[0].turn_idx, 0);
        assert_eq!(u[1].turn_idx, 1);
        assert_eq!(u[0].input_tokens, 1);
        assert_eq!(u[1].input_tokens, 2);
    }

    #[test]
    fn started_at_uses_preceding_message_ts() {
        let jsonl = r#"{"type":"user","timestamp":"2026-05-02T10:00:00Z"}
{"type":"assistant","timestamp":"2026-05-02T10:00:05Z","message":{"usage":{"input_tokens":0,"output_tokens":0}}}"#;
        let u = parse_usages(jsonl);
        assert!(u[0].started_at_ms.unwrap() < u[0].ended_at_ms.unwrap());
        assert_eq!(
            u[0].ended_at_ms.unwrap() - u[0].started_at_ms.unwrap(),
            5_000
        );
    }
}
