//! `spice install symbols`: verify the installed macOS executable and dSYM
//! against the receipt, optionally symbolizing one `sample(1)` report.

use std::path::Path;

use crate::environment_fault;
use serde::Serialize;
use spice::ambient::Ambient;
use spice::install::{
    DwarfdumpLookup, SampleError, SampleSymbolization, SymbolVerification, SymbolVerificationError,
    symbolize_sample, verify_symbols,
};
use spice::repository::Repository;
use wire::{Outcome, Refusal, RefusalDiagnostic, Reply, ReplyPosition};

/// The verified installed symbol pair and, when asked, one resolved sample.
#[derive(Serialize)]
pub(crate) struct InstalledSymbols {
    verification: SymbolVerification,
    #[serde(skip_serializing_if = "Option::is_none")]
    sample: Option<SampleSymbolization>,
}

pub(crate) fn run(matches: &clap::ArgMatches, ambient: &Ambient) -> Outcome<InstalledSymbols> {
    let working = match std::env::current_dir() {
        Ok(working) => working,
        Err(error) => {
            return environment_fault("install::working-directory-unavailable", error.to_string());
        }
    };
    let repository = match Repository::discover(&working) {
        Ok(repository) => repository,
        Err(error) => {
            return environment_fault(spice::repository::ResolveError::REASON, error.to_string());
        }
    };
    let verification = match verify_symbols(&repository) {
        Ok(verification) => verification,
        Err(SymbolVerificationError::Install(error)) => {
            return crate::install_command::failure(error);
        }
        Err(error) => {
            return Outcome::Refusal(Refusal::bare(RefusalDiagnostic::new(
                "install::symbols-unverified".to_owned(),
                Some("installed-symbols".to_owned()),
                Some(
                    "installed executable and dSYM agreeing on SHA-256, UUID, paths, and Git provenance"
                        .to_owned(),
                ),
                Some(error.to_string()),
            )));
        }
    };
    let sample = match matches.get_one::<String>("sample") {
        None => None,
        Some(sample) => {
            let environment = match ambient.environment() {
                Ok(environment) => environment,
                Err(error) => {
                    return environment_fault("install::environment-failed", error.to_string());
                }
            };
            let lookup = DwarfdumpLookup::new(working, environment);
            match symbolize_sample(&repository, &verification, Path::new(sample), &lookup) {
                Ok(symbolized) => Some(symbolized),
                Err(SampleError::Lookup { address, source }) => {
                    return environment_fault(
                        "install::symbol-lookup-failed",
                        format!("lookup of {address} failed: {source}"),
                    );
                }
                Err(error) => {
                    return Outcome::Refusal(Refusal::bare(RefusalDiagnostic::new(
                        "install::sample-refused".to_owned(),
                        Some("sample-report".to_owned()),
                        Some("a bounded sample(1) report of the installed executable".to_owned()),
                        Some(error.to_string()),
                    )));
                }
            }
        }
    };
    Outcome::Reply(Reply {
        position: ReplyPosition::Direct {},
        body: InstalledSymbols {
            verification,
            sample,
        },
    })
}
