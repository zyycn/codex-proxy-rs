//! 校验插件管理声明的路由路径与资源内容类型

use std::collections::BTreeSet;

use gateway_admin::model::AdminError;
use gateway_plugin_sdk::call::management::ManagementRegistration;

pub(super) fn registration(value: &ManagementRegistration) -> Result<(), AdminError> {
    if value.routes.len() > 64
        || value.resources.len() > 128
        || value.pages.len() > 16
        || value.callbacks.len() > 16
        || (value.routes.is_empty() && value.resources.is_empty() && value.callbacks.is_empty())
    {
        return Err(AdminError::invalid("插件管理声明为空或超过条数限制"));
    }
    let mut routes = BTreeSet::new();
    for route in &value.routes {
        if !matches!(
            route.method.as_str(),
            "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE"
        ) || !path(&route.path)
            || !routes.insert((&route.method, &route.path))
            || route.request_content_types.len() > 16
            || route.response_content_types.is_empty()
            || route.response_content_types.len() > 16
            || !content_types(&route.request_content_types)
            || !content_types(&route.response_content_types)
        {
            return Err(AdminError::invalid("插件管理路由声明无效或重复"));
        }
    }
    let mut resources = BTreeSet::new();
    let mut callbacks = BTreeSet::new();
    for callback in &value.callbacks {
        if !path(&callback.path)
            || !callbacks.insert(&callback.path)
            || callback.response_content_types.is_empty()
            || callback.response_content_types.len() > 16
            || !content_types(&callback.response_content_types)
        {
            return Err(AdminError::invalid("插件登录回调路径或内容类型无效"));
        }
    }
    for resource in &value.resources {
        if !path(&resource.path) || !resources.insert(&resource.path) {
            return Err(AdminError::invalid("插件资源路径无效或重复"));
        }
    }
    let mut pages = BTreeSet::new();
    for page in &value.pages {
        if !path(&page.id)
            || page.id.contains('/')
            || page.id.len() > 64
            || !pages.insert(&page.id)
            || page.title.is_empty()
            || page.title.len() > 128
            || page.title.chars().any(char::is_control)
            || page.description.as_ref().is_some_and(|description| {
                description.trim().is_empty()
                    || description.len() > 512
                    || description.chars().any(char::is_control)
            })
            || !resources.contains(&page.entry)
            || page
                .icon
                .as_ref()
                .is_some_and(|icon| !resources.contains(icon))
        {
            return Err(AdminError::invalid(
                "插件页面标识、标题、说明或资源引用无效",
            ));
        }
    }
    Ok(())
}

fn content_types(values: &[String]) -> bool {
    values.iter().collect::<BTreeSet<_>>().len() == values.len()
        && values.iter().all(|value| content_type(value))
}

pub(super) fn content_type(value: &str) -> bool {
    let mime = value.strip_suffix("; charset=utf-8").unwrap_or(value);
    value.len() <= 128
        && mime.split_once('/').is_some_and(|(kind, subtype)| {
            [kind, subtype].iter().all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&byte))
            })
        })
}

fn path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 512
        && value.split('/').all(|part| {
            !part.is_empty()
                && !matches!(part, "." | "..")
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        })
}
