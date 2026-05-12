//! Claude Code hook events.
//!
//! Source of truth: <https://code.claude.com/docs/en/hooks>. Variants are
//! PascalCase to match Claude's wire names exactly.

/// All Claude Code hook events `cc-ledger` understands. Variants we don't
/// install today are still listed so `from_wire_name` round-trips correctly
/// if Claude ever fires one of them at us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeHookEvent {
    SessionStart,
    Setup,
    UserPromptSubmit,
    UserPromptExpansion,
    PreToolUse,
    PermissionRequest,
    PermissionDenied,
    PostToolUse,
    PostToolUseFailure,
    PostToolBatch,
    SubagentStart,
    SubagentStop,
    TaskCreated,
    TaskCompleted,
    Stop,
    StopFailure,
    TeammateIdle,
    InstructionsLoaded,
    ConfigChange,
    CwdChanged,
    FileChanged,
    PreCompact,
    PostCompact,
    Notification,
    Elicitation,
    ElicitationResult,
    WorktreeCreate,
    WorktreeRemove,
    SessionEnd,
}

impl ClaudeHookEvent {
    /// Wire name as written by Claude Code in `hook_event_name` and used as
    /// the key in `settings.hooks.<name>`.
    pub fn as_wire_name(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::Setup => "Setup",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::UserPromptExpansion => "UserPromptExpansion",
            Self::PreToolUse => "PreToolUse",
            Self::PermissionRequest => "PermissionRequest",
            Self::PermissionDenied => "PermissionDenied",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::PostToolBatch => "PostToolBatch",
            Self::SubagentStart => "SubagentStart",
            Self::SubagentStop => "SubagentStop",
            Self::TaskCreated => "TaskCreated",
            Self::TaskCompleted => "TaskCompleted",
            Self::Stop => "Stop",
            Self::StopFailure => "StopFailure",
            Self::TeammateIdle => "TeammateIdle",
            Self::InstructionsLoaded => "InstructionsLoaded",
            Self::ConfigChange => "ConfigChange",
            Self::CwdChanged => "CwdChanged",
            Self::FileChanged => "FileChanged",
            Self::PreCompact => "PreCompact",
            Self::PostCompact => "PostCompact",
            Self::Notification => "Notification",
            Self::Elicitation => "Elicitation",
            Self::ElicitationResult => "ElicitationResult",
            Self::WorktreeCreate => "WorktreeCreate",
            Self::WorktreeRemove => "WorktreeRemove",
            Self::SessionEnd => "SessionEnd",
        }
    }

    /// Kebab-case form of the event name, used as the positional argument
    /// to `cc-ledger hook <event>`. Mirrors `as_wire_name` 1:1.
    pub fn as_kebab_name(self) -> &'static str {
        match self {
            Self::SessionStart => "session-start",
            Self::Setup => "setup",
            Self::UserPromptSubmit => "user-prompt-submit",
            Self::UserPromptExpansion => "user-prompt-expansion",
            Self::PreToolUse => "pre-tool-use",
            Self::PermissionRequest => "permission-request",
            Self::PermissionDenied => "permission-denied",
            Self::PostToolUse => "post-tool-use",
            Self::PostToolUseFailure => "post-tool-use-failure",
            Self::PostToolBatch => "post-tool-batch",
            Self::SubagentStart => "subagent-start",
            Self::SubagentStop => "subagent-stop",
            Self::TaskCreated => "task-created",
            Self::TaskCompleted => "task-completed",
            Self::Stop => "stop",
            Self::StopFailure => "stop-failure",
            Self::TeammateIdle => "teammate-idle",
            Self::InstructionsLoaded => "instructions-loaded",
            Self::ConfigChange => "config-change",
            Self::CwdChanged => "cwd-changed",
            Self::FileChanged => "file-changed",
            Self::PreCompact => "pre-compact",
            Self::PostCompact => "post-compact",
            Self::Notification => "notification",
            Self::Elicitation => "elicitation",
            Self::ElicitationResult => "elicitation-result",
            Self::WorktreeCreate => "worktree-create",
            Self::WorktreeRemove => "worktree-remove",
            Self::SessionEnd => "session-end",
        }
    }

    /// Inverse of `as_kebab_name`. Returns `None` for unknown events.
    pub fn from_kebab_name(s: &str) -> Option<Self> {
        Some(match s {
            "session-start" => Self::SessionStart,
            "setup" => Self::Setup,
            "user-prompt-submit" => Self::UserPromptSubmit,
            "user-prompt-expansion" => Self::UserPromptExpansion,
            "pre-tool-use" => Self::PreToolUse,
            "permission-request" => Self::PermissionRequest,
            "permission-denied" => Self::PermissionDenied,
            "post-tool-use" => Self::PostToolUse,
            "post-tool-use-failure" => Self::PostToolUseFailure,
            "post-tool-batch" => Self::PostToolBatch,
            "subagent-start" => Self::SubagentStart,
            "subagent-stop" => Self::SubagentStop,
            "task-created" => Self::TaskCreated,
            "task-completed" => Self::TaskCompleted,
            "stop" => Self::Stop,
            "stop-failure" => Self::StopFailure,
            "teammate-idle" => Self::TeammateIdle,
            "instructions-loaded" => Self::InstructionsLoaded,
            "config-change" => Self::ConfigChange,
            "cwd-changed" => Self::CwdChanged,
            "file-changed" => Self::FileChanged,
            "pre-compact" => Self::PreCompact,
            "post-compact" => Self::PostCompact,
            "notification" => Self::Notification,
            "elicitation" => Self::Elicitation,
            "elicitation-result" => Self::ElicitationResult,
            "worktree-create" => Self::WorktreeCreate,
            "worktree-remove" => Self::WorktreeRemove,
            "session-end" => Self::SessionEnd,
            _ => return None,
        })
    }

    /// Inverse of `as_wire_name`. Returns `None` for unknown events; the
    /// hook handler treats unknown events as no-ops.
    pub fn from_wire_name(s: &str) -> Option<Self> {
        // One match table; if `as_wire_name` and this drift, tests catch it.
        Some(match s {
            "SessionStart" => Self::SessionStart,
            "Setup" => Self::Setup,
            "UserPromptSubmit" => Self::UserPromptSubmit,
            "UserPromptExpansion" => Self::UserPromptExpansion,
            "PreToolUse" => Self::PreToolUse,
            "PermissionRequest" => Self::PermissionRequest,
            "PermissionDenied" => Self::PermissionDenied,
            "PostToolUse" => Self::PostToolUse,
            "PostToolUseFailure" => Self::PostToolUseFailure,
            "PostToolBatch" => Self::PostToolBatch,
            "SubagentStart" => Self::SubagentStart,
            "SubagentStop" => Self::SubagentStop,
            "TaskCreated" => Self::TaskCreated,
            "TaskCompleted" => Self::TaskCompleted,
            "Stop" => Self::Stop,
            "StopFailure" => Self::StopFailure,
            "TeammateIdle" => Self::TeammateIdle,
            "InstructionsLoaded" => Self::InstructionsLoaded,
            "ConfigChange" => Self::ConfigChange,
            "CwdChanged" => Self::CwdChanged,
            "FileChanged" => Self::FileChanged,
            "PreCompact" => Self::PreCompact,
            "PostCompact" => Self::PostCompact,
            "Notification" => Self::Notification,
            "Elicitation" => Self::Elicitation,
            "ElicitationResult" => Self::ElicitationResult,
            "WorktreeCreate" => Self::WorktreeCreate,
            "WorktreeRemove" => Self::WorktreeRemove,
            "SessionEnd" => Self::SessionEnd,
            _ => return None,
        })
    }

    /// Tool events have a `tool_name` field; their hook block uses the `"*"`
    /// catch-all matcher (matches any tool). Non-tool events use the empty
    /// matcher `""`.
    pub fn is_tool_event(self) -> bool {
        matches!(
            self,
            Self::PreToolUse
                | Self::PermissionRequest
                | Self::PermissionDenied
                | Self::PostToolUse
                | Self::PostToolUseFailure
                | Self::PostToolBatch
                | Self::Elicitation
                | Self::ElicitationResult
        )
    }

    /// Catch-all matcher string for this event. `"*"` for tool events, `""`
    /// for the rest.
    pub fn catch_all_matcher(self) -> &'static str {
        if self.is_tool_event() {
            "*"
        } else {
            ""
        }
    }
}

/// Curated set of events `cc-ledger` installs into `~/.claude/settings.json`.
/// One row per event the ledger actually consumes; extend as new phases land.
pub const INSTALL_EVENTS: &[ClaudeHookEvent] = &[
    ClaudeHookEvent::SessionStart,
    ClaudeHookEvent::SessionEnd,
    ClaudeHookEvent::PreToolUse,
    ClaudeHookEvent::PostToolUse,
    ClaudeHookEvent::PostToolUseFailure,
    ClaudeHookEvent::Stop,
    ClaudeHookEvent::SubagentStop,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_wire_names() {
        for ev in INSTALL_EVENTS {
            assert_eq!(
                ClaudeHookEvent::from_wire_name(ev.as_wire_name()),
                Some(*ev)
            );
        }
    }

    #[test]
    fn round_trip_kebab_names() {
        for ev in INSTALL_EVENTS {
            assert_eq!(
                ClaudeHookEvent::from_kebab_name(ev.as_kebab_name()),
                Some(*ev)
            );
        }
    }

    #[test]
    fn unknown_kebab_name_is_none() {
        assert_eq!(ClaudeHookEvent::from_kebab_name("not-an-event"), None);
    }

    #[test]
    fn matchers_match_expected_pattern() {
        assert_eq!(ClaudeHookEvent::PreToolUse.catch_all_matcher(), "*");
        assert_eq!(ClaudeHookEvent::SessionStart.catch_all_matcher(), "");
    }

    #[test]
    fn unknown_wire_name_is_none() {
        assert_eq!(ClaudeHookEvent::from_wire_name("NotAnEvent"), None);
    }
}
