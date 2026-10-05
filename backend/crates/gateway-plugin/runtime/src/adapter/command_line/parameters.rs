//! 插件命令行参数定义校验、输入解析与帮助文本生成

use std::collections::{BTreeMap, BTreeSet};

use gateway_admin::model::AdminError;
use gateway_plugin_sdk::call::management::{
    CommandDescriptor, CommandInvocation, CommandParameterType, CommandValue,
};

const MAXIMUM_ARGUMENT_BYTES: usize = 64 * 1024;

pub(super) fn validate(command: &CommandDescriptor) -> Result<(), AdminError> {
    let mut names = BTreeSet::new();
    if !identifier(&command.name)
        || matches!(
            command.name.as_str(),
            "help" | "version" | "serve" | "plugin"
        )
        || !description(&command.description)
        || command.parameters.len() > 64
    {
        return Err(AdminError::invalid("插件命令描述不合法"));
    }
    for parameter in &command.parameters {
        if !identifier(&parameter.name)
            || matches!(parameter.name.as_str(), "help" | "version")
            || !description(&parameter.description)
            || !names.insert(&parameter.name)
            || parameter.default.as_ref().is_some_and(|value| {
                !matches_type(value, parameter.value_type)
                    || parameter.required
                    || matches!(value, CommandValue::String(value) if value.len() > MAXIMUM_ARGUMENT_BYTES)
            })
        {
            return Err(AdminError::invalid("插件命令参数、默认值或名称冲突"));
        }
    }
    Ok(())
}

pub(super) fn parse(
    command: &CommandDescriptor,
    arguments: &[String],
) -> Result<CommandInvocation, AdminError> {
    if arguments.len() > 128
        || arguments
            .iter()
            .try_fold(0usize, |size, argument| size.checked_add(argument.len()))
            .is_none_or(|size| size > MAXIMUM_ARGUMENT_BYTES)
    {
        return Err(AdminError::invalid("插件命令参数超过大小限制"));
    }
    let mut values = BTreeMap::new();
    let mut arguments = arguments.iter().peekable();
    while let Some(argument) = arguments.next() {
        let argument = argument.strip_prefix("--").ok_or_else(invalid_argument)?;
        let (name, inline) = argument
            .split_once('=')
            .map_or((argument, None), |(name, value)| (name, Some(value)));
        let parameter = command
            .parameters
            .iter()
            .find(|parameter| parameter.name == name)
            .ok_or_else(invalid_argument)?;
        if values.contains_key(name) {
            return Err(invalid_argument());
        }
        let value = if parameter.value_type == CommandParameterType::Bool && inline.is_none() {
            CommandValue::Bool(true)
        } else {
            let text = inline
                .or_else(|| arguments.next().map(String::as_str))
                .ok_or_else(invalid_argument)?;
            parse_value(text, parameter.value_type).ok_or_else(invalid_argument)?
        };
        values.insert(name.to_owned(), value);
    }
    for parameter in &command.parameters {
        if !values.contains_key(&parameter.name) {
            if let Some(default) = &parameter.default {
                values.insert(parameter.name.clone(), default.clone());
            } else if parameter.required {
                return Err(AdminError::invalid(format!(
                    "缺少必填插件参数 --{}",
                    parameter.name
                )));
            }
        }
    }
    Ok(CommandInvocation {
        name: command.name.clone(),
        arguments: values,
    })
}

pub(super) fn help(command: &CommandDescriptor) -> String {
    let mut output = format!("{}\n", command.description);
    for parameter in &command.parameters {
        let value_type = match parameter.value_type {
            CommandParameterType::Bool => "bool",
            CommandParameterType::String => "string",
            CommandParameterType::Int => "int",
            CommandParameterType::Int64 => "int64",
            CommandParameterType::Float64 => "float64",
            CommandParameterType::Duration => "duration",
        };
        output.push_str(&format!(
            "  --{} <{}>  {}",
            parameter.name, value_type, parameter.description
        ));
        if parameter.required {
            output.push_str(" [必填]");
        }
        if let Some(value) = &parameter.default {
            output.push_str(" [默认: ");
            if parameter.sensitive {
                output.push_str("已隐藏");
            } else if let Ok(value) = serde_json::to_value(value) {
                output.push_str(&value["value"].to_string());
                if parameter.value_type == CommandParameterType::Duration {
                    output.push_str("ns");
                }
            }
            output.push(']');
        }
        output.push('\n');
    }
    output.push_str("  --help  仅显示帮助，不执行命令\n");
    output
}

fn parse_value(value: &str, kind: CommandParameterType) -> Option<CommandValue> {
    Some(match kind {
        CommandParameterType::Bool => CommandValue::Bool(match value {
            "true" => true,
            "false" => false,
            _ => return None,
        }),
        CommandParameterType::String => CommandValue::String(value.to_owned()),
        CommandParameterType::Int => CommandValue::Int(value.parse().ok()?),
        CommandParameterType::Int64 => CommandValue::Int64(value.parse().ok()?),
        CommandParameterType::Float64 => {
            let number: f64 = value.parse().ok()?;
            if !number.is_finite() {
                return None;
            }
            CommandValue::Float64(number)
        }
        CommandParameterType::Duration => CommandValue::Duration(super::duration::parse(value)?),
    })
}

fn matches_type(value: &CommandValue, kind: CommandParameterType) -> bool {
    match (value, kind) {
        (CommandValue::Bool(_), CommandParameterType::Bool)
        | (CommandValue::String(_), CommandParameterType::String)
        | (CommandValue::Int(_), CommandParameterType::Int)
        | (CommandValue::Int64(_), CommandParameterType::Int64)
        | (CommandValue::Duration(_), CommandParameterType::Duration) => true,
        (CommandValue::Float64(value), CommandParameterType::Float64) => value.is_finite(),
        _ => false,
    }
}

fn identifier(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn description(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}

fn invalid_argument() -> AdminError {
    // 不回显用户参数；即使未声明 sensitive，参数也可能包含凭据
    AdminError::invalid("插件命令参数未知、重复、缺值或类型不正确；请查看 --help")
}
