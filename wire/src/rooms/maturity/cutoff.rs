//! The heard-question cutoff a Design's current statement must meet before its
//! maturity can steep.

/// Heard current questions a Design naming `questions` needs before it can
/// steep: `min(N, ceil(2 ln(1 + N)))`. Every question of one to four, four of
/// five or six, five of seven to eleven, six of twelve to nineteen, and seven
/// of twenty to thirty-two; the share declines as statements grow.
#[must_use]
pub fn design_heard_required(questions: usize) -> usize {
    // For every question count a statement can hold, 2 ln(1 + N) stays at
    // least 4.7e-6 from an integer, far beyond f64 error, so this ceiling is
    // exact.
    let questions_f64 = f64::from(u32::try_from(questions).unwrap_or(u32::MAX));
    let cutoff = (2.0 * questions_f64.ln_1p()).ceil() as usize;
    cutoff.min(questions)
}

#[cfg(test)]
mod tests {
    use super::design_heard_required;

    #[test]
    fn heard_cutoff_is_the_doubled_log_whole_question_count() {
        for (questions, required) in [
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 3),
            (4, 4),
            (5, 4),
            (6, 4),
            (7, 5),
            (8, 5),
            (9, 5),
            (10, 5),
            (11, 5),
            (12, 6),
            (20, 7),
        ] {
            assert_eq!(
                design_heard_required(questions),
                required,
                "{questions} questions"
            );
        }
        // The last count at each raw ceil(2 ln(1 + N)), computed exactly
        // offline; the next count needs one more heard question.
        for (last, cutoff) in [
            (3, 3),
            (6, 4),
            (11, 5),
            (19, 6),
            (32, 7),
            (53, 8),
            (89, 9),
            (147, 10),
            (243, 11),
            (402, 12),
            (664, 13),
            (1_095, 14),
            (1_807, 15),
            (2_979, 16),
            (4_913, 17),
            (8_102, 18),
            (13_358, 19),
            (22_025, 20),
            (36_314, 21),
            (59_873, 22),
        ] {
            assert_eq!(design_heard_required(last), cutoff, "{last} questions");
            assert_eq!(
                design_heard_required(last + 1),
                cutoff + 1,
                "{} questions",
                last + 1
            );
        }
    }
}
