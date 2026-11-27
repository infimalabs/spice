use std::env;
use std::io;
use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments = env::args_os().skip(1).collect();
    match prepared_artifact::prepared_artifact(arguments, &mut io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("prepared-artifact: {error}");
            ExitCode::FAILURE
        }
    }
}
