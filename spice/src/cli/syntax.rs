//! Compilation of one generated contract syntax into Clap arguments.

use clap::{
    Arg, ArgAction, Command,
    builder::{OsStringValueParser, PossibleValuesParser},
};

use super::Contract;

pub(super) fn register_syntax(
    mut command: Command,
    contract: &Contract,
) -> Result<Command, String> {
    let path = contract.path.join(" ");
    let Some(tail) = contract.syntax.strip_prefix(path.as_str()) else {
        return Err(format!("does not begin with registered path {path:?}"));
    };
    if !tail.is_empty() && !tail.starts_with(' ') {
        return Err(format!("does not delimit registered path {path:?}"));
    }

    let tokens = syntax_tokens(tail.trim())?;
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        if token == "--" {
            let Some(value) = tokens.get(index + 1).copied() else {
                return Err("trailing -- has no command operand".to_owned());
            };
            if index + 2 != tokens.len() || is_optional(value) || value.starts_with("--") {
                return Err("-- must be followed by one final command operand".to_owned());
            }
            command = command.arg(positional(value, true, true)?);
            index += 2;
            continue;
        }

        if let Some(group) = optional_contents(token) {
            command = register_optional_group(command, group)?;
        } else if token.starts_with("--") {
            let Some(value) = tokens
                .get(index + 1)
                .copied()
                .filter(|value| !value.starts_with("--") && !is_optional(value))
            else {
                return Err(format!("required option needs one value: {token}"));
            };
            command = command.arg(option(&[token, value])?.required(true));
            index += 1;
        } else {
            command = command.arg(positional(token, true, false)?);
        }
        index += 1;
    }
    Ok(command)
}

pub(super) fn syntax_tokens(input: &'static str) -> Result<Vec<&'static str>, String> {
    let mut tokens = Vec::new();
    let mut depth = 0_u32;
    let mut start = None;

    for (offset, character) in input.char_indices() {
        if character.is_whitespace() && depth == 0 {
            if let Some(token_start) = start.take() {
                tokens.push(&input[token_start..offset]);
            }
            continue;
        }
        if start.is_none() {
            start = Some(offset);
        }
        match character {
            '[' => depth = depth.saturating_add(1),
            ']' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| format!("unmatched ] at byte {offset}"))?;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err("unclosed [ group".to_owned());
    }
    if let Some(token_start) = start {
        tokens.push(&input[token_start..]);
    }
    Ok(tokens)
}

pub(super) fn is_optional(token: &str) -> bool {
    token.starts_with('[') && token.ends_with(']')
}

fn optional_contents(token: &'static str) -> Option<&'static str> {
    token.strip_prefix('[')?.strip_suffix(']')
}

fn register_optional_group(mut command: Command, group: &'static str) -> Result<Command, String> {
    let tokens = syntax_tokens(group)?;
    let Some(first) = tokens.first().copied() else {
        return Err("empty optional group".to_owned());
    };

    if first.starts_with("--") {
        if tokens.len() == 1 && first.contains("|--") {
            let alternatives = first.split('|').collect::<Vec<_>>();
            let names = alternatives
                .iter()
                .map(|alternative| option_name(alternative))
                .collect::<Result<Vec<_>, _>>()?;
            for (alternative, name) in alternatives.into_iter().zip(names.iter().copied()) {
                let conflicts = names.iter().copied().filter(|other| *other != name);
                command = command.arg(option(&[alternative])?.conflicts_with_all(conflicts));
            }
            return Ok(command);
        }
        return Ok(command.arg(option(&tokens)?));
    }

    for token in tokens {
        if let Some(nested) = optional_contents(token) {
            let nested_tokens = syntax_tokens(nested)?;
            if nested_tokens.len() != 1 {
                return Err(format!(
                    "nested optional group must contain one operand: {token}"
                ));
            }
            command = command.arg(positional(nested_tokens[0], false, false)?);
        } else if token.starts_with("--") {
            return Err(format!(
                "option follows positional in optional group: {group}"
            ));
        } else {
            command = command.arg(positional(token, false, false)?);
        }
    }
    Ok(command)
}

fn positional(syntax: &'static str, required: bool, trailing: bool) -> Result<Arg, String> {
    let (name, variadic) = match syntax.strip_suffix("...") {
        Some(name) => (name, true),
        None => (syntax, false),
    };
    if name.is_empty() || name.starts_with('-') {
        return Err(format!("invalid positional operand: {syntax}"));
    }

    let mut argument = Arg::new(name).value_name(name).required(required);
    if variadic || trailing {
        argument = argument.num_args(1..);
    }
    if trailing {
        argument = argument
            .last(true)
            .allow_hyphen_values(true)
            .value_parser(OsStringValueParser::new());
    } else if name == "MESSAGE_FILE" {
        // Git hook paths are native filesystem operands. On Unix they need
        // not be UTF-8, so this one typed positional must retain `OsString`
        // all the way from Clap to `Path`.
        argument = argument.value_parser(OsStringValueParser::new());
    } else if name.contains('|') && name.chars().any(char::is_lowercase) {
        argument = argument.value_parser(PossibleValuesParser::new(name.split('|')));
    }
    Ok(argument)
}

fn option(parts: &[&'static str]) -> Result<Arg, String> {
    let option_syntax = parts
        .first()
        .copied()
        .ok_or_else(|| "option group is empty".to_owned())?;
    let name = option_name(option_syntax)?;
    let mut argument = Arg::new(name).long(name);

    // `--flag[=VALUE]`: the value attaches only through `=`, so a following
    // operand is never swallowed as the flag's value.
    if let Some(value) = option_syntax
        .split_once("[=")
        .and_then(|(_, rest)| rest.strip_suffix(']'))
    {
        if parts.len() != 1 || value.is_empty() || value.contains([' ', '[', '.', '|']) {
            return Err(format!(
                "invalid optional equals option syntax: {option_syntax}"
            ));
        }
        return Ok(argument
            .value_name(value)
            .num_args(0..=1)
            .require_equals(true)
            .action(ArgAction::Set));
    }
    if let Some((_, value)) = option_syntax.split_once('=') {
        if parts.len() != 1 || value.is_empty() {
            return Err(format!("invalid equals option syntax: {option_syntax}"));
        }
        // Clap brackets every value name, so the registry's own pair goes.
        let value = value
            .strip_prefix('<')
            .and_then(|value| value.strip_suffix('>'))
            .unwrap_or(value);
        return Ok(argument
            .value_name(value)
            .num_args(1)
            .require_equals(true)
            .action(ArgAction::Set));
    }

    match parts {
        [_] => Ok(argument.action(ArgAction::SetTrue)),
        [_, value] if is_optional(value) => {
            let value = optional_contents(value)
                .filter(|value| !value.is_empty() && !value.contains([' ', '[', '.', '|']))
                .ok_or_else(|| format!("invalid optional option value: {value}"))?;
            Ok(argument
                .value_name(value)
                .num_args(0..=1)
                .action(ArgAction::Set))
        }
        [_, value] if !value.starts_with('-') && !is_optional(value) => {
            let (value, repeated) = value
                .strip_suffix("...")
                .map_or((*value, false), |value| (value, true));
            if value.is_empty() {
                return Err(format!("invalid repeated option value: {}", parts[1]));
            }
            argument = argument.value_name(value).num_args(1).action(if repeated {
                ArgAction::Append
            } else {
                ArgAction::Set
            });
            if value.contains('|') && value.chars().any(char::is_lowercase) {
                argument = argument.value_parser(PossibleValuesParser::new(value.split('|')));
            }
            Ok(argument)
        }
        _ => Err(format!("invalid option group: {}", parts.join(" "))),
    }
}

fn option_name(option: &'static str) -> Result<&'static str, String> {
    let body = option
        .strip_prefix("--")
        .ok_or_else(|| format!("long option must begin with --: {option}"))?;
    let name = body.split(['[', '=']).next().unwrap_or(body);
    if name.is_empty() {
        return Err("long option has no name".to_owned());
    }
    Ok(name)
}
