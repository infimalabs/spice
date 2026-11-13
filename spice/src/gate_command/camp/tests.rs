use super::*;

const INTERVAL: Duration = Duration::from_secs(30);
const STEP: Duration = Duration::from_secs(2);
const CAMP_MEASUREMENTS: usize = 100;
const CAMP_DISTRIBUTIONS: usize = 4;
const CAMP_LOCI: usize = 16;
const CAMP_LINE_BUDGET: usize = 10;

#[test]
fn a_long_camp_campaign_names_its_shape_and_beats_with_measurement_progress() {
    let start = Instant::now();
    let mut campaign = CampCampaign::new(INTERVAL, start);
    let base = CampProgress {
        metric: None,
        completed: 0,
        total: CAMP_MEASUREMENTS,
        distributions: CAMP_DISTRIBUTIONS,
        loci: CAMP_LOCI,
    };
    let mut lines = vec![(
        start,
        campaign
            .observe(base, start)
            .expect("the initial observation names the campaign"),
    )];
    let mut now = start;
    let metrics = (0..base.total)
        .map(|index| format!("rust.metric-{index}"))
        .collect::<Vec<_>>();
    for (index, metric) in metrics.iter().enumerate() {
        campaign.observe(
            CampProgress {
                metric: Some(metric),
                completed: index,
                ..base
            },
            now,
        );
        for _ in 0..STEP.as_secs() {
            now += TICK;
            lines.extend(campaign.heartbeat(now).map(|line| (now, line)));
        }
        campaign.observe(
            CampProgress {
                metric: Some(metric),
                completed: index + 1,
                ..base
            },
            now,
        );
    }

    assert_eq!(
        lines[0].1,
        "campaign: 4 distributions, 16 loci, 100 measurements"
    );
    assert_eq!(lines[1].1, "14/100 measured; rust.metric-14 running 30.0s");
    for pair in lines.windows(2) {
        assert_eq!(
            pair[1].0.duration_since(pair[0].0),
            INTERVAL,
            "heartbeat cadence moved at {}",
            pair[1].1
        );
    }
    assert!(
        lines.len() < CAMP_LINE_BUDGET,
        "{} lines for 100 measurements",
        lines.len()
    );
}
