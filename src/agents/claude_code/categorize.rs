//! Codeburn-style activity classifier (port of `codeburn/src/classifier.ts`).
//!
//! Pure functions: takes a user message and the agent's tool usage, returns
//! one of 13 categories. Three-tier cascade — tool patterns → keyword
//! refinement → fallback heuristics on the user text.

use regex::Regex;
use std::sync::OnceLock;

/// Bump when the cascade rules or category set change. Backfill rebuilds
/// rows whose `classifier_version` is below this.
pub const CLASSIFIER_VERSION: i64 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Coding,
    Debugging,
    Feature,
    Refactoring,
    Testing,
    Exploration,
    Planning,
    Delegation,
    Git,
    BuildDeploy,
    Conversation,
    Brainstorming,
    General,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Coding => "coding",
            Category::Debugging => "debugging",
            Category::Feature => "feature",
            Category::Refactoring => "refactoring",
            Category::Testing => "testing",
            Category::Exploration => "exploration",
            Category::Planning => "planning",
            Category::Delegation => "delegation",
            Category::Git => "git",
            Category::BuildDeploy => "build/deploy",
            Category::Conversation => "conversation",
            Category::Brainstorming => "brainstorming",
            Category::General => "general",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Tool,
    Keyword,
    Fallback,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Tool => "tool",
            Tier::Keyword => "keyword",
            Tier::Fallback => "fallback",
        }
    }
}

/// Aggregate of everything the classifier needs from one codeburn-style turn.
#[derive(Debug, Default, Clone)]
pub struct TurnFacts<'a> {
    pub user_message: &'a str,
    /// All tool names used across all assistant API calls in this turn.
    pub tools: Vec<&'a str>,
    /// All bash command strings (post-`extractBashCommands`-style normalization
    /// is fine; we just need the user-visible command text for regex matching).
    pub bash_commands: Vec<&'a str>,
    pub has_plan_mode: bool,
    pub has_agent_spawn: bool,
}

fn is_edit_tool(t: &str) -> bool {
    matches!(
        t,
        "Edit" | "Write" | "FileEditTool" | "FileWriteTool" | "NotebookEdit" | "cursor:edit"
    )
}

fn is_read_tool(t: &str) -> bool {
    matches!(
        t,
        "Read" | "Grep" | "Glob" | "FileReadTool" | "GrepTool" | "GlobTool"
    )
}

pub fn is_bash_tool(t: &str) -> bool {
    matches!(t, "Bash" | "BashTool" | "PowerShellTool")
}

fn is_task_tool(t: &str) -> bool {
    matches!(
        t,
        "TaskCreate"
            | "TaskUpdate"
            | "TaskGet"
            | "TaskList"
            | "TaskOutput"
            | "TaskStop"
            | "TodoWrite"
    )
}

fn is_search_tool(t: &str) -> bool {
    matches!(t, "WebSearch" | "WebFetch" | "ToolSearch")
}

fn is_mcp_tool(t: &str) -> bool {
    t.starts_with("mcp__")
}

fn is_skill_tool(t: &str) -> bool {
    t == "Skill"
}

// Compile-once regexes. `regex::Regex::new` is cheap but not free — and the
// classifier runs once per agent turn during backfill (~3K times for the
// initial backfill pass).
fn re(pat: &str) -> Regex {
    Regex::new(pat).expect("classifier regex")
}

macro_rules! lazy_re {
    ($name:ident, $pat:expr) => {
        fn $name() -> &'static Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| re($pat))
        }
    };
}

lazy_re!(
    test_re,
    r"(?i)\b(test|pytest|vitest|jest|mocha|spec|coverage|npm\s+test|npx\s+vitest|npx\s+jest)\b"
);
lazy_re!(
    git_re,
    r"(?i)\bgit\s+(push|pull|commit|merge|rebase|checkout|branch|stash|log|diff|status|add|reset|cherry-pick|tag)\b"
);
lazy_re!(
    build_re,
    r"(?i)\b(npm\s+run\s+build|npm\s+publish|pip\s+install|docker|deploy|make\s+build|npm\s+run\s+dev|npm\s+start|pm2|systemctl|brew|cargo\s+build)\b"
);
lazy_re!(
    install_re,
    r"(?i)\b(npm\s+install|pip\s+install|brew\s+install|apt\s+install|cargo\s+add)\b"
);
lazy_re!(
    debug_re,
    r"(?i)\b(fix|bug|error|broken|failing|crash|issue|debug|traceback|exception|stack\s*trace|not\s+working|wrong|unexpected|status\s+code|404|500|401|403)\b"
);
lazy_re!(
    feature_re,
    r"(?i)\b(add|create|implement|new|build|feature|introduce|set\s*up|scaffold|generate|make\s+(?:a|me|the)|write\s+(?:a|me|the))\b"
);
lazy_re!(
    refactor_re,
    r"(?i)\b(refactor|clean\s*up|rename|reorganize|simplify|extract|restructure|move|migrate|split)\b"
);
lazy_re!(
    brainstorm_re,
    r"(?i)\b(brainstorm|idea|what\s+if|explore|think\s+about|approach|strategy|design|consider|how\s+should|what\s+would|opinion|suggest|recommend)\b"
);
lazy_re!(
    research_re,
    r"(?i)\b(research|investigate|look\s+into|find\s+out|check|search|analyze|review|understand|explain|how\s+does|what\s+is|show\s+me|list|compare)\b"
);
lazy_re!(
    file_re,
    r"(?i)\.(py|js|ts|tsx|jsx|json|yaml|yml|toml|sql|sh|go|rs|java|rb|php|css|html|md|csv|xml)\b"
);
lazy_re!(
    script_re,
    r"(?i)\b(run\s+\S+\.\w+|execute|scrip?t|curl|api\s+\S+|endpoint|request\s+url|fetch\s+\S+|query|database|db\s+\S+)\b"
);
lazy_re!(url_re, r"(?i)https?://\S+");

fn any_match(re: &Regex, haystacks: &[&str]) -> bool {
    haystacks.iter().any(|s| re.is_match(s))
}

fn classify_by_tool_pattern(facts: &TurnFacts<'_>) -> Option<Category> {
    if facts.tools.is_empty() {
        return None;
    }
    if facts.has_plan_mode {
        return Some(Category::Planning);
    }
    if facts.has_agent_spawn {
        return Some(Category::Delegation);
    }

    let has_edits = facts.tools.iter().any(|t| is_edit_tool(t));
    let has_reads = facts.tools.iter().any(|t| is_read_tool(t));
    let has_bash = facts.tools.iter().any(|t| is_bash_tool(t));
    let has_tasks = facts.tools.iter().any(|t| is_task_tool(t));
    let has_search = facts.tools.iter().any(|t| is_search_tool(t));
    let has_mcp = facts.tools.iter().any(|t| is_mcp_tool(t));
    let has_skill = facts.tools.iter().any(|t| is_skill_tool(t));

    if has_bash && !has_edits {
        // Match against the user message AND the actual bash command strings.
        // Codeburn matches user_message only; we extend to bash_commands so a
        // turn whose only signal is the bash invocation itself still classifies.
        let hay: Vec<&str> = std::iter::once(facts.user_message)
            .chain(facts.bash_commands.iter().copied())
            .collect();
        if any_match(test_re(), &hay) {
            return Some(Category::Testing);
        }
        if any_match(git_re(), &hay) {
            return Some(Category::Git);
        }
        if any_match(build_re(), &hay) {
            return Some(Category::BuildDeploy);
        }
        if any_match(install_re(), &hay) {
            return Some(Category::BuildDeploy);
        }
    }

    if has_edits {
        return Some(Category::Coding);
    }

    if has_bash && has_reads {
        return Some(Category::Exploration);
    }
    if has_bash {
        return Some(Category::Coding);
    }

    if has_search || has_mcp {
        return Some(Category::Exploration);
    }
    if has_reads && !has_edits {
        return Some(Category::Exploration);
    }
    if has_tasks && !has_edits {
        return Some(Category::Planning);
    }
    if has_skill {
        return Some(Category::General);
    }

    None
}

fn refine_by_keywords(category: Category, user_message: &str) -> Category {
    match category {
        Category::Coding => {
            if debug_re().is_match(user_message) {
                Category::Debugging
            } else if refactor_re().is_match(user_message) {
                Category::Refactoring
            } else if feature_re().is_match(user_message) {
                Category::Feature
            } else {
                Category::Coding
            }
        }
        Category::Exploration => {
            if research_re().is_match(user_message) {
                Category::Exploration
            } else if debug_re().is_match(user_message) {
                Category::Debugging
            } else {
                Category::Exploration
            }
        }
        c => c,
    }
}

fn classify_conversation(user_message: &str) -> Category {
    if brainstorm_re().is_match(user_message) {
        Category::Brainstorming
    } else if research_re().is_match(user_message) {
        Category::Exploration
    } else if debug_re().is_match(user_message) {
        Category::Debugging
    } else if feature_re().is_match(user_message) {
        Category::Feature
    } else if file_re().is_match(user_message) || script_re().is_match(user_message) {
        Category::Coding
    } else if url_re().is_match(user_message) {
        Category::Exploration
    } else {
        Category::Conversation
    }
}

/// Three-tier cascade: tool pattern → keyword refinement → fallback.
pub fn classify_turn(facts: &TurnFacts<'_>) -> (Category, Tier) {
    if facts.tools.is_empty() {
        return (classify_conversation(facts.user_message), Tier::Fallback);
    }
    match classify_by_tool_pattern(facts) {
        Some(cat) => {
            let refined = refine_by_keywords(cat, facts.user_message);
            let tier = if refined == cat {
                Tier::Tool
            } else {
                Tier::Keyword
            };
            (refined, tier)
        }
        None => (classify_conversation(facts.user_message), Tier::Fallback),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>(msg: &'a str, tools: &'a [&'a str]) -> TurnFacts<'a> {
        TurnFacts {
            user_message: msg,
            tools: tools.to_vec(),
            ..TurnFacts::default()
        }
    }

    #[test]
    fn no_tools_with_brainstorm_keywords() {
        let (cat, tier) = classify_turn(&facts("brainstorm a new approach", &[]));
        assert_eq!(cat, Category::Brainstorming);
        assert_eq!(tier, Tier::Fallback);
    }

    #[test]
    fn no_tools_with_url_falls_back_to_exploration() {
        let (cat, _) = classify_turn(&facts("look at https://example.com please", &[]));
        assert_eq!(cat, Category::Exploration);
    }

    #[test]
    fn edit_tools_yield_coding() {
        let (cat, tier) = classify_turn(&facts("update the file", &["Edit", "Write"]));
        assert_eq!(cat, Category::Coding);
        assert_eq!(tier, Tier::Tool);
    }

    #[test]
    fn coding_refined_to_debugging_by_user_msg() {
        let (cat, tier) = classify_turn(&facts("fix the failing test", &["Edit"]));
        assert_eq!(cat, Category::Debugging);
        assert_eq!(tier, Tier::Keyword);
    }

    #[test]
    fn coding_refined_to_refactoring() {
        let (cat, _) = classify_turn(&facts("refactor this module", &["Edit"]));
        assert_eq!(cat, Category::Refactoring);
    }

    #[test]
    fn coding_refined_to_feature() {
        let (cat, _) = classify_turn(&facts("add a new endpoint", &["Edit"]));
        assert_eq!(cat, Category::Feature);
    }

    #[test]
    fn read_only_tools_yield_exploration() {
        let (cat, _) = classify_turn(&facts("check this", &["Read", "Grep"]));
        assert_eq!(cat, Category::Exploration);
    }

    #[test]
    fn task_tools_alone_yield_planning() {
        let (cat, _) = classify_turn(&facts("plan it out", &["TodoWrite"]));
        assert_eq!(cat, Category::Planning);
    }

    #[test]
    fn mcp_tool_yields_exploration() {
        let (cat, _) = classify_turn(&facts("check notion", &["mcp__notion__fetch"]));
        assert_eq!(cat, Category::Exploration);
    }

    #[test]
    fn plan_mode_overrides_other_signals() {
        let f = TurnFacts {
            user_message: "do the thing",
            tools: vec!["Edit", "EnterPlanMode"],
            has_plan_mode: true,
            ..TurnFacts::default()
        };
        let (cat, _) = classify_turn(&f);
        assert_eq!(cat, Category::Planning);
    }

    #[test]
    fn agent_spawn_yields_delegation() {
        let f = TurnFacts {
            user_message: "spawn an agent",
            tools: vec!["Agent"],
            has_agent_spawn: true,
            ..TurnFacts::default()
        };
        let (cat, _) = classify_turn(&f);
        assert_eq!(cat, Category::Delegation);
    }

    #[test]
    fn bash_with_test_command_yields_testing() {
        let f = TurnFacts {
            user_message: "run something",
            tools: vec!["Bash"],
            bash_commands: vec!["pytest tests/"],
            ..TurnFacts::default()
        };
        assert_eq!(classify_turn(&f).0, Category::Testing);
    }

    #[test]
    fn bash_with_git_command_yields_git() {
        let f = TurnFacts {
            user_message: "ship it",
            tools: vec!["Bash"],
            bash_commands: vec!["git push origin main"],
            ..TurnFacts::default()
        };
        assert_eq!(classify_turn(&f).0, Category::Git);
    }

    #[test]
    fn bash_with_build_command_yields_build_deploy() {
        let f = TurnFacts {
            user_message: "compile",
            tools: vec!["Bash"],
            bash_commands: vec!["cargo build --release"],
            ..TurnFacts::default()
        };
        assert_eq!(classify_turn(&f).0, Category::BuildDeploy);
    }

    #[test]
    fn bash_only_with_no_match_yields_coding() {
        let f = TurnFacts {
            user_message: "do it",
            tools: vec!["Bash"],
            bash_commands: vec!["echo hello"],
            ..TurnFacts::default()
        };
        assert_eq!(classify_turn(&f).0, Category::Coding);
    }

    #[test]
    fn skill_alone_yields_general() {
        let (cat, _) = classify_turn(&facts("use skill", &["Skill"]));
        assert_eq!(cat, Category::General);
    }
}
