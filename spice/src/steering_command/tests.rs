//! Dispatch parity for the steering selection lifted out of [`super::run`].
//!
//! Every steering contract the parser can select is driven through
//! [`super::select`] here, so rerouting one arm to another
//! [`SelectedCommand`] reds this module rather than reaching the resident.

use std::ffi::OsString;

use clap::ArgMatches;
use wire::{CanonicalUuid, FaultClass, Outcome};

use super::{SelectedCommand, select};

const LANE: &str = "0a2f5b6c-1d3e-4f50-8a91-b2c3d4e5f607";
const DIRECTIVE: &str = "1b3f6c7d-2e4f-4051-9b02-c3d4e5f60718";

/// Parse one argv the way the binary does, keeping the leaf matches owned.
fn parse(argv: &[&str]) -> (&'static crate::cli::Contract, ArgMatches) {
    let matches = crate::cli::command()
        .try_get_matches_from(argv)
        .expect("the declared steering argv parses");
    let contract = crate::cli::selected_contract(&matches).expect("the argv selects a contract");
    let leaf = crate::cli::selected_matches(&matches).clone();
    (contract, leaf)
}

/// Select one steering contract with lane selectors resolved as the canonical
/// UUIDs these fixtures write, so selection parity needs no resident.
fn chosen<'a>(
    leaf: &'a ArgMatches,
    contract: &crate::cli::Contract,
    lane_environment: Option<OsString>,
) -> Result<(SelectedCommand<'a>, Option<CanonicalUuid>), Outcome<wire::intent::CommandAnswer>> {
    let lanes = |written: &str, reason: &str| super::uuid_operand_text(written, reason);
    select(leaf, contract, lane_environment, &lanes)
}

/// Take the selection a steering contract answers, or fail with its path.
fn selection<'a>(
    leaf: &'a ArgMatches,
    contract: &crate::cli::Contract,
    lane_environment: Option<OsString>,
) -> (SelectedCommand<'a>, Option<CanonicalUuid>) {
    match chosen(leaf, contract, lane_environment) {
        Ok(selection) => selection,
        Err(_) => panic!("{:?} selects a steering command", contract.path),
    }
}

fn lane() -> CanonicalUuid {
    LANE.parse().expect("the fixture lane is canonical")
}

fn directive() -> CanonicalUuid {
    DIRECTIVE
        .parse()
        .expect("the fixture directive is canonical")
}

#[test]
fn steer_selects_send_with_no_lane_context() {
    let (contract, leaf) = parse(&["spice", "steer", LANE, "hold the line"]);
    let (selected, context) = selection(&leaf, contract, None);

    assert!(matches!(
        selected,
        SelectedCommand::SteerSend { lane, message }
            if lane == self::lane() && message == "hold the line"
    ));
    assert_eq!(context, None);
}

#[test]
fn steer_from_a_seated_lane_is_refused_as_non_operator_direction() {
    let (contract, leaf) = parse(&["spice", "steer", LANE, "closing smoke"]);
    let sender = "2c4a7d8e-3f50-4162-8c13-d4e5f6071829";

    let Err(Outcome::Refusal(refused)) = chosen(&leaf, contract, Some(OsString::from(sender)))
    else {
        panic!("a seated lane cannot author direction");
    };
    assert_eq!(refused.reason(), "steer::operator-required");
    assert_eq!(
        refused
            .diagnostic()
            .and_then(|diagnostic| diagnostic.observed()),
        Some(format!("lane:{sender}").as_str())
    );
}

#[test]
fn steer_cancel_selects_cancel_rather_than_recover() {
    let (contract, leaf) = parse(&["spice", "steer", "cancel", DIRECTIVE]);
    let (selected, context) = selection(&leaf, contract, None);

    assert!(matches!(
        selected,
        SelectedCommand::SteerCancel { directive } if directive == self::directive()
    ));
    assert_eq!(context, None);
}

#[test]
fn steer_resend_selects_live_resend_rather_than_lapsed_recovery() {
    let (contract, leaf) = parse(&["spice", "steer", "resend", DIRECTIVE]);
    let selected = chosen(&leaf, contract, None).expect("resend selects");
    assert!(matches!(
        selected.0,
        SelectedCommand::SteerResend { directive } if directive == self::directive()
    ));
}

#[test]
fn steer_recover_selects_recover_rather_than_cancel() {
    let (contract, leaf) = parse(&["spice", "steer", "recover", DIRECTIVE]);
    let (selected, context) = selection(&leaf, contract, None);

    assert!(matches!(
        selected,
        SelectedCommand::SteerRecover { directive } if directive == self::directive()
    ));
    assert_eq!(context, None);
}

#[test]
fn inbox_carries_its_lane_operand_and_the_shell_context() {
    let (contract, leaf) = parse(&["spice", "inbox", LANE]);
    let (selected, context) = selection(&leaf, contract, Some(OsString::from(LANE)));

    assert!(matches!(
        selected,
        SelectedCommand::SteerInbox { lane: Some(lane) } if lane == self::lane()
    ));
    assert_eq!(context, Some(lane()));
}

#[test]
fn a_non_steering_contract_faults_rather_than_selecting_a_steering_command() {
    let (contract, leaf) = parse(&["spice", "down"]);

    let Err(Outcome::Fault(fault)) = chosen(&leaf, contract, None) else {
        panic!("a non-steering contract faults");
    };
    assert_eq!(fault.class, FaultClass::Environment);
    assert_eq!(fault.reason, "steer::dispatch-invalid");
}
