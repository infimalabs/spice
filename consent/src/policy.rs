use std::collections::BTreeSet;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};
use wire::Name;

/// Name the posture answers to, which no rule may take.
pub const POSTURE: &str = "posture";

/// Name the operator answers to, which no rule may take.
pub const OPERATOR: &str = "operator";

/// What an authority answers for a request it decides.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Answer {
    /// The action proceeds.
    Allow,
    /// The action is refused, loudly, naming the rule that refused it.
    Deny,
    /// The action waits for the operator while reversible work continues.
    #[default]
    Queue,
}

/// The named authority that answered a request.
///
/// The posture and the operator are their own variants rather than rules
/// spelled `posture` and `operator`, so a configured rule cannot impersonate
/// either one in a fact an operator reads. [`Policy::new`] refuses both
/// spellings for the same reason: exactly one thing answers to each name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Authority {
    /// The first rule whose tool pattern matched the operation.
    Rule(String),
    /// No rule matched, so the configured posture answered.
    Posture,
    /// The operator answered a held request directly.
    Operator,
}

impl Authority {
    /// The name every consent fact cites for this authority.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Rule(name) => name,
            Self::Posture => POSTURE,
            Self::Operator => OPERATOR,
        }
    }
}

/// One named rule in a policy's stated order.
///
/// The name is the rule's identity because facts cite it and budgets meter per
/// name, so renaming a rule creates a new rule with a fresh budget rather than
/// carrying the old one forward.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rule {
    name: Name,
    tool: String,
    answer: Answer,
    budget: Option<u32>,
}

impl Rule {
    /// Admit one named rule.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] when the name is empty or reserved, when the
    /// tool pattern is empty, or when a rule that cannot grant carries a
    /// budget.
    pub fn new(
        name: impl Into<String>,
        tool: impl Into<String>,
        answer: Answer,
        budget: Option<u32>,
    ) -> Result<Self, PolicyError> {
        let name = name.into();
        let tool = tool.into();
        if name.trim().is_empty() {
            return Err(PolicyError::UnnamedRule);
        }
        if name == POSTURE || name == OPERATOR {
            return Err(PolicyError::ReservedName);
        }
        let name = name
            .parse::<Name>()
            .map_err(|_| PolicyError::MalformedName { rule: name })?;
        if tool.is_empty() {
            return Err(PolicyError::EmptyPattern {
                rule: name.to_string(),
            });
        }
        if budget.is_some() && answer != Answer::Allow {
            return Err(PolicyError::UngrantableBudget {
                rule: name.to_string(),
            });
        }
        if budget == Some(0) {
            return Err(PolicyError::ZeroBudget {
                rule: name.to_string(),
            });
        }
        Ok(Self {
            name,
            tool,
            answer,
            budget,
        })
    }

    /// The rule's identity.
    #[must_use]
    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    /// Exact operation or glob this rule answers for.
    #[must_use]
    pub fn tool(&self) -> &str {
        &self.tool
    }

    /// What this rule answers.
    #[must_use]
    pub const fn answer(&self) -> Answer {
        self.answer
    }

    /// Grants this rule allows before one queued renewal.
    #[must_use]
    pub const fn budget(&self) -> Option<u32> {
        self.budget
    }

    /// Whether this rule's pattern covers the operation.
    #[must_use]
    pub fn matches(&self, operation: &str) -> bool {
        glob_matches(self.tool.as_bytes(), operation.as_bytes())
    }
}

/// Whether a glob covers this operation, where `*` spans any run of bytes.
///
/// Operations are command words rather than paths, so nothing here is a
/// separator and `*` has no boundary to respect. A pattern without `*` is an
/// exact string, which is why an operator can read the table by eye.
fn glob_matches(pattern: &[u8], operation: &[u8]) -> bool {
    let mut pattern_index = 0;
    let mut operation_index = 0;
    let mut star: Option<usize> = None;
    let mut resume = 0;
    while operation_index < operation.len() {
        match pattern.get(pattern_index) {
            Some(b'*') => {
                star = Some(pattern_index);
                pattern_index += 1;
                resume = operation_index;
            }
            Some(byte) if *byte == operation[operation_index] => {
                pattern_index += 1;
                operation_index += 1;
            }
            _ => match star {
                // Backtrack to the last star and let it cover one byte more.
                Some(index) => {
                    pattern_index = index + 1;
                    resume += 1;
                    operation_index = resume;
                }
                None => return false,
            },
        }
    }
    pattern[pattern_index..].iter().all(|byte| *byte == b'*')
}

/// Named rules in a stated order, with a posture for what none of them match.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Policy {
    posture: Answer,
    rules: Vec<Rule>,
}

impl Policy {
    /// Admit an ordered rule table under one posture.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] when two rules share a name, since the name is
    /// the identity a fact cites and a budget meters against.
    pub fn new(posture: Answer, rules: Vec<Rule>) -> Result<Self, PolicyError> {
        let mut seen = BTreeSet::new();
        for rule in &rules {
            if !seen.insert(rule.name()) {
                return Err(PolicyError::DuplicateName {
                    rule: rule.name().to_owned(),
                });
            }
        }
        Ok(Self { posture, rules })
    }

    /// Disposition taken when no rule matches.
    #[must_use]
    pub const fn posture(&self) -> Answer {
        self.posture
    }

    /// The rules in the order they decide.
    #[must_use]
    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Decide one operation: the first matching rule answers, else the posture.
    #[must_use]
    pub fn decide(&self, operation: &str) -> Decision {
        self.rules
            .iter()
            .find(|rule| rule.matches(operation))
            .map_or(
                Decision {
                    authority: Authority::Posture,
                    answer: self.posture,
                    bound: None,
                },
                |rule| Decision {
                    authority: Authority::Rule(rule.name().to_owned()),
                    answer: rule.answer(),
                    bound: rule.budget(),
                },
            )
    }
}

/// Which authority answered an operation, and what it answered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decision {
    authority: Authority,
    answer: Answer,
    bound: Option<u32>,
}

impl Decision {
    /// The rule or posture that answered, by the name facts cite.
    #[must_use]
    pub const fn authority(&self) -> &Authority {
        &self.authority
    }

    /// The disposition that authority answered with.
    #[must_use]
    pub const fn answer(&self) -> Answer {
        self.answer
    }

    /// Grants the answering rule allows before one queued renewal.
    #[must_use]
    pub const fn bound(&self) -> Option<u32> {
        self.bound
    }
}

/// A rule table that could not be admitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PolicyError {
    /// A rule carried no name for facts to cite.
    UnnamedRule,
    /// A rule took a name an authority other than a rule answers to.
    ReservedName,
    /// A rule identity was not one lower-case ASCII word.
    MalformedName { rule: String },
    /// A rule matched nothing because its pattern was empty.
    EmptyPattern { rule: String },
    /// A rule that never grants carried a grant budget.
    UngrantableBudget { rule: String },
    /// A budget of no grants would queue every request, which the posture says.
    ZeroBudget { rule: String },
    /// Two rules claimed one identity, so facts and budgets could not tell them apart.
    DuplicateName { rule: String },
}

impl PolicyError {
    /// The stable reason this table was refused.
    #[must_use]
    pub const fn reason(&self) -> &'static str {
        match self {
            Self::UnnamedRule => "consent rule is unnamed",
            Self::ReservedName => "consent rule takes a reserved authority name",
            Self::MalformedName { .. } => "consent rule name is malformed",
            Self::EmptyPattern { .. } => "consent rule matches nothing",
            Self::UngrantableBudget { .. } => "consent rule cannot spend a budget",
            Self::ZeroBudget { .. } => "consent budget grants nothing",
            Self::DuplicateName { .. } => "consent rules share one name",
        }
    }

    /// The step that admits this table.
    #[must_use]
    pub fn remedy(&self) -> String {
        match self {
            Self::UnnamedRule => {
                "give the rule a name; every consent fact cites the rule that decided".to_owned()
            }
            Self::ReservedName => format!(
                concat!(
                    "rename the rule; `{POSTURE}` names the disposition unmatched requests take ",
                    "and `{OPERATOR}` names the operator answering a held request"
                ),
                POSTURE = POSTURE,
                OPERATOR = OPERATOR,
            ),
            Self::MalformedName { rule } => format!(
                "rename `{rule}` with one lower-case word; rule names are stable fact identities"
            ),
            Self::EmptyPattern { rule } => {
                format!("give `{rule}` an exact operation or a glob such as `git push*`")
            }
            Self::UngrantableBudget { rule } => format!(
                concat!(
                    "drop the budget from `{rule}` or answer `allow`; ",
                    "a budget bounds grants and this rule grants none"
                ),
                rule = rule,
            ),
            Self::ZeroBudget { rule } => {
                format!("raise `{rule}`'s budget above zero or answer `queue` outright")
            }
            Self::DuplicateName { rule } => format!(
                concat!(
                    "rename one of the rules called `{rule}`; ",
                    "a name is a rule's identity and its budget"
                ),
                rule = rule,
            ),
        }
    }

    /// The rule this refusal names, when one rule is at fault.
    #[must_use]
    pub fn rule(&self) -> Option<&str> {
        match self {
            Self::UnnamedRule | Self::ReservedName => None,
            Self::MalformedName { rule }
            | Self::EmptyPattern { rule }
            | Self::UngrantableBudget { rule }
            | Self::ZeroBudget { rule }
            | Self::DuplicateName { rule } => Some(rule),
        }
    }

    /// This refusal in the one shape every spice surface refuses with.
    #[must_use]
    pub fn refusal(&self) -> wire::Refusal {
        wire::Refusal::bare(
            wire::RefusalDiagnostic::new(self.reason(), self.rule().map(str::to_owned), None, None)
                .with_detail(self.remedy()),
        )
    }
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.rule() {
            Some(rule) => write!(formatter, "{}: `{rule}`; {}", self.reason(), self.remedy()),
            None => write!(formatter, "{}; {}", self.reason(), self.remedy()),
        }
    }
}

impl Error for PolicyError {}
