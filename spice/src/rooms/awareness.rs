//! The request text every room surface presents for a Design.
//!
//! Matching completed work to open Designs scores with the shared statement
//! engine in [`board::similarity`].
use std::fmt;

use super::CITABLE_KEYS;

/// Most rendered bytes of whole open questions one Design request carries.
pub const DESIGN_REQUEST_QUESTIONS_BYTES: usize = 2 * 1024;

/// The stance every Design request body opens with, whether it asks about
/// question items or about the whole Design, and what a charge's sign weighs.
const START_FROM_NO: &str = "Start from no: this Design and each question must earn its place \
     from evidence. A charge weighs whether the question's answer changes what gets built: \
     positive says it does, negative says it does not, and it never votes for one alternative \
     the question names. A negative charge is the expected answer when a question has not \
     earned its place; charge every question negative when the Design itself should not exist.";
/// What answers a Design request: a view the lane already holds is complete
/// without new research or citation, citation is what a 0 or ±2 charge adds,
/// and a leave is for a lane that, having considered its matched work, holds
/// no view.
const ANSWERING: &str = "Your view from work you have done and knowledge you already hold is a \
     complete answer: a brief ±1 charge needs no citation, no new research, and no novel \
     finding, and it is the expected answer whenever you hold a view. A 0 or ±2 charge adds a \
     real source it rests on that you have checked. Agreeing with a recorded charge counts as \
     your own view, never as added volume. When, having considered the matched work and what \
     you already know, you genuinely hold no view, a leave is a complete answer too; lacking a \
     citable source or a new finding is never a reason to leave.";
/// What separates the matched earlier work from the lane's current assignment.
const PRIOR_WORK: &str = "That matched work is your earlier experience, not your current \
     assignment: this request asks what it taught you, so a different current task is no reason \
     to leave. Consider it before concluding you hold no view.";
/// The bound every Design request keeps: a view is asked for, nothing more.
const CONSULTATION: &str = "This consultation is limited to what you already know and the \
     questions offered: it grants no claim, starts no work, and asks for no implementation or \
     new investigation.";
/// The event at which a Design request evaluated one session profile, in the
/// words shared by the delivered request and the awareness read.
pub fn notice_opportunity_text(
    opportunity: &wire::rooms::DesignNoticeOpportunity,
) -> impl fmt::Display + '_ {
    NoticeOpportunityText(opportunity)
}

struct NoticeOpportunityText<'a>(&'a wire::rooms::DesignNoticeOpportunity);

impl fmt::Display for NoticeOpportunityText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        use wire::rooms::DesignNoticeOpportunity as Opportunity;

        match self.0 {
            Opportunity::TaskCompletion { task, .. } => {
                write!(formatter, "completion of task {task}")
            }
            Opportunity::TaskClaim {
                task,
                claim,
                terminal,
            } => match terminal {
                Some(terminal) => write!(
                    formatter,
                    "terminal operation {terminal} under task {task} claim {claim}"
                ),
                None => write!(formatter, "retained task {task} claim {claim}"),
            },
            Opportunity::Conversation { .. } => {
                formatter.write_str("a settled conversation message")
            }
            Opportunity::DesignStatement => formatter.write_str("this Design statement"),
            Opportunity::ProfileReady { .. } => formatter.write_str("the completed lane profile"),
            Opportunity::SessionResumed { .. } => formatter.write_str("this resumed session"),
        }
    }
}

/// The retained support for a Design request, without its authored excerpt, in
/// the words shared by the delivered request and the awareness read.
pub fn notice_evidence_text(
    evidence: &wire::rooms::DesignNoticeEvidence,
) -> impl fmt::Display + '_ {
    NoticeEvidenceText(evidence)
}

struct NoticeEvidenceText<'a>(&'a wire::rooms::DesignNoticeEvidence);

impl fmt::Display for NoticeEvidenceText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        use wire::rooms::DesignNoticeEvidence as Evidence;

        let code = |seq: wire::Seq| wire::SequenceCode::new(seq.get());
        match self.0 {
            Evidence::Task {
                task,
                statement,
                completed,
                ..
            } => write!(
                formatter,
                "task {task} (statement revision {}, completion {})",
                code(*statement),
                code(*completed),
            ),
            Evidence::TaskClaim {
                task,
                statement,
                claim,
                terminal,
                ..
            } => {
                write!(
                    formatter,
                    "task {task} (statement revision {}, claim {claim}",
                    code(*statement)
                )?;
                if let Some(terminal) = terminal {
                    write!(formatter, ", terminal operation {}", code(*terminal))?;
                }
                formatter.write_str(")")
            }
            Evidence::Conversation { source } => {
                let locus = &source.locus;
                match locus.source {
                    Some(tape) => write!(
                        formatter,
                        "transcript {tape} member {} byte {} length {}",
                        locus.member, locus.offset, locus.length
                    ),
                    None => write!(
                        formatter,
                        "transcript member {} byte {}",
                        locus.member, locus.offset
                    ),
                }
            }
        }
    }
}

/// The exact source and authored excerpt of the work that supported one Design
/// request, phrased from the supporting claim's phase or as conversation.
fn supporting_work(
    request: &wire::intent::DesignRequest,
    source: &wire::rooms::DesignNoticeEvidence,
    excerpt: &str,
) -> String {
    use wire::rooms::DesignNoticeEvidence as Evidence;

    let evidence = notice_evidence_text(source);
    match source {
        Evidence::Task { task, support, .. } | Evidence::TaskClaim { task, support, .. } => {
            format!(
                "you {} {evidence}: \"{excerpt}\". Read it with `spice task show {task} \
                 --history`.",
                support.verb(),
            )
        }
        Evidence::Conversation { source } => {
            let locus = &source.locus;
            let Some(tape) = locus.source else {
                return format!("you explained this in conversation: \"{excerpt}\" ({evidence}).");
            };
            let command = shell_words::join([
                "spice".to_owned(),
                "session".to_owned(),
                "context".to_owned(),
                request.session.identity.clone(),
                "--lane".to_owned(),
                request.lane.to_string(),
                "--vendor".to_owned(),
                request.session.vendor.to_string(),
                "--stream".to_owned(),
                wire::body::TASK_CREATION_CONTEXT_STREAM.to_owned(),
                "--source".to_owned(),
                tape.to_string(),
                "--member".to_owned(),
                locus.member.to_string(),
                "--offset".to_owned(),
                locus.offset.to_string(),
            ]);
            format!(
                "you explained this in conversation: \"{excerpt}\" ({evidence}). Read it with \
                 `{command}`."
            )
        }
    }
}

/// One Design invitation as every command surface presents it.
#[must_use]
pub fn request_text(request: &wire::intent::DesignRequest) -> String {
    let design = &request.design;
    let opportunity = notice_opportunity_text(&request.opportunity);
    let support = supporting_work(request, &request.evidence, &request.excerpt);
    let cast = match (request.admission.reach(), request.asks.as_slice()) {
        (wire::rooms::DesignReach::Wide, _) => {
            " Its Design is not yet steeped, so this match was cast slightly wider than usual: \
             a lower relevance floor admitted it."
                .to_owned()
        }
        (wire::rooms::DesignReach::Item, [ask]) => format!(
            " Your work matched this Design's question {} in particular, and this request asks \
             for your view on that one question.",
            ask.handle
        ),
        (wire::rooms::DesignReach::Item, [_, _, ..]) => {
            " Your work matched these questions of this Design in particular, and this request \
             asks for your view on them, the one needing feedback most first."
                .to_owned()
        }
        _ => String::new(),
    };
    let head = format!(
        "Pitch: {}\nDesign request {design} \"{}\" at statement revision {} matched your work \
         on: {}.{cast} The profile was evaluated at {opportunity}.\nSupporting work: {support} \
         {PRIOR_WORK}\n",
        request.pitch,
        request.title,
        request.statement,
        request.terms.join(", "),
    );
    if !request.asks.is_empty() {
        return format!("{head}{}", item_request(request));
    }
    let items = wire::rooms::StatementItem::each(&request.questions).collect::<Vec<_>>();
    let mut questions = String::new();
    for (item, question) in items.iter().zip(&request.questions) {
        questions.push_str(&format!("\n- {} {question}", item.handle(&items)));
    }
    if questions.is_empty() {
        questions.push_str("\n- none recorded");
    }
    let breadth = breadth(request.questions.len());
    format!(
        "{head}{START_FROM_NO} {ANSWERING}\nOpen questions:{questions}\n{breadth}\
         Read its room with `spice room history {design}` (`spice://task/{design}/room`). \
         Answer exactly once with `spice room contribute {design} --charge=C --question HEX \
         [--cite KEY...] TEXT`, where HEX is the handle before an open question, C from -2 to +2 \
         weighs it, and a 0 or ±2 charge cites {CITABLE_KEYS}; pose a \
         new question with `spice room contribute {design} --ask TEXT`; or decline with `spice \
         room leave {design} --clause TEXT`. Until one answer publishes, `spice task next` \
         returns this request instead of allocating. {CONSULTATION} You hear this room's later \
         posts until that one answer, a contribution or a leave, ends the request and its \
         listening.",
    )
}

/// What one answer owes `count` listed questions: a sentence once there is a
/// choice, nothing for one.
fn breadth(count: usize) -> &'static str {
    if count < 2 {
        return "";
    }
    "A brief comment on any one, several, or all of these questions is welcome; one answer \
     settles this request, and you need not weigh every question.\n"
}

/// The body of a request that asks about question items: each item with its
/// standing, the one contribute form, and the request's own supporting work
/// offered as a citation.
fn item_request(request: &wire::intent::DesignRequest) -> String {
    let design = &request.design;
    let mut questions = String::new();
    for ask in &request.asks {
        let lanes = match ask.lanes {
            0 => String::new(),
            lanes => format!(", {lanes} lane{}", plural(lanes as u64)),
        };
        questions.push_str(&format!(
            "\n- {} ({}{lanes}): \"{}\"",
            ask.handle,
            ask.state.label(),
            ask.text
        ));
        if let Some(support) = &ask.support {
            questions.push_str(&format!(
                "\n  Supporting work for {}: {} {}",
                ask.handle,
                supporting_work(request, &support.evidence, &support.excerpt),
                citation(&support.evidence),
            ));
        }
    }
    let recharge = if request
        .asks
        .iter()
        .any(|ask| ask.state == wire::rooms::DesignItemState::Contested)
    {
        "\nBoth signs weigh a contested question: if you charged it before, a strength-2 \
         re-charge citing what your position rests on would show where it now stands; if not, \
         your own charge is what can help settle it."
    } else {
        ""
    };
    let cite = if request.asks.iter().all(|ask| ask.support.is_none()) {
        citation(&request.evidence)
    } else {
        String::new()
    };
    let (these, breadth) = match request.asks.len() {
        1 => ("this question", ""),
        count => ("these questions", breadth(count)),
    };
    format!(
        "{START_FROM_NO} {ANSWERING}\nWeigh {these}:{questions}{recharge}\n{breadth}\
         Read its room with `spice room history {design}` (`spice://task/{design}/room`). Weigh \
         the question whose handle is HEX with `spice room contribute {design} --charge=C \
         --question HEX [--cite KEY...] TEXT`, where C runs from -2 to +2 and a 0 or ±2 \
         charge cites what it rests on; or decline with `spice room leave {design} --clause \
         TEXT`. Until one answer publishes, `spice task next` returns this request instead of \
         allocating; one answer settles it. {CONSULTATION} You hear this room's later posts \
         until that answer, a contribution or a leave, ends the request and its listening. \
         {cite}",
    )
}

fn citation(evidence: &wire::rooms::DesignNoticeEvidence) -> String {
    use wire::rooms::DesignNoticeEvidence as Evidence;
    match evidence {
        Evidence::Task { task, .. } | Evidence::TaskClaim { task, .. } => {
            format!("This work cites as `--cite {task}` when a charge rests on it.")
        }
        Evidence::Conversation { .. } => format!(
            "The matched conversation is not citable. An opinion needs none; a 0 or ±2 charge \
             cites {CITABLE_KEYS} instead."
        ),
    }
}

fn plural(count: u64) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// The longest in-order prefix of whole open questions that fits one request.
#[must_use]
pub fn request_questions(questions: &[String]) -> Vec<String> {
    let mut used: usize = 0;
    questions
        .iter()
        .take_while(|question| {
            let rendered = question.len().saturating_add(3);
            let fits = used.saturating_add(rendered) <= DESIGN_REQUEST_QUESTIONS_BYTES;
            if fits {
                used += rendered;
            }
            fits
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_questions_keep_only_the_whole_in_order_prefix_that_fits() {
        let first = "a".repeat(DESIGN_REQUEST_QUESTIONS_BYTES - 3);
        let questions = [first.clone(), "does not fit".to_owned(), "short".to_owned()];
        assert_eq!(request_questions(&questions), [first]);
    }
}
