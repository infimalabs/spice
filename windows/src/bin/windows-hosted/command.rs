use std::ffi::OsString;
use std::path::Path;

pub(super) fn declare(arguments: &[OsString]) -> Result<bool, String> {
    if arguments.get(1).and_then(|value| value.to_str()) != Some("--declare-native-stage") {
        return Ok(false);
    }
    let [_, _, target, root] = arguments else {
        return Err("usage: windows-hosted --declare-native-stage TARGET ROOT".to_owned());
    };
    let target = target
        .to_str()
        .ok_or_else(|| "native target is not Unicode".to_owned())?;
    windows_execution::hosted::declare_native_stage(target, Path::new(root))
        .map_err(|error| error.to_string())?;
    Ok(true)
}

pub(super) fn smoke_argv(arguments: &[OsString]) -> Result<(), String> {
    let mut exit = 0;
    for argument in arguments {
        let argument = argument
            .to_str()
            .ok_or_else(|| "smoke argv is not Unicode".to_owned())?;
        if let Some(code) = argument.strip_prefix("--exit=") {
            exit = code
                .parse::<i32>()
                .map_err(|_| "invalid smoke exit".to_owned())?;
        } else {
            println!("{argument}");
        }
    }
    std::process::exit(exit);
}
