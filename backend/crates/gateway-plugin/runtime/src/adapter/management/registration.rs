//! 将插件管理声明编译为页面、路由与资源入口

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use gateway_admin::model::{
    AdminError,
    plugins::{
        instances::PluginInstance,
        management::{
            PluginManagementPage, PluginManagementResource, PluginManagementRoute,
            PluginManagementTarget, PluginManagementView,
        },
    },
};
use gateway_plugin_sdk::{Capability, Stage, call::management::ManagementRegistration};

use crate::{RpcSession, ValidatedPackage, callback::PluginCallbacks};

use super::{MAXIMUM_BODY_BYTES, ManagementEntry, validation};

pub(crate) async fn prepare(
    instance: &PluginInstance,
    package: &ValidatedPackage,
    session: Arc<RpcSession>,
    callbacks: Arc<PluginCallbacks>,
) -> Result<Option<ManagementEntry>, AdminError> {
    let manifest = package.manifest();
    let Some(declaration) = manifest.contributes.get(&Capability::Management) else {
        return Ok(None);
    };
    if declaration.stages != [Stage::Management] {
        return Err(AdminError::invalid("管理扩展必须声明 management 阶段"));
    }
    let reply = session
        .call(
            "management.register",
            session.context(Stage::Registration, Duration::from_secs(5)),
            serde_json::json!({}),
            Vec::new(),
        )
        .await
        .map_err(|_| AdminError::invalid("插件管理扩展注册失败"))?;
    if reply.result != serde_json::json!({}) || reply.payload.len() > 64 * 1024 {
        return Err(AdminError::invalid("插件管理注册信封无效或过大"));
    }
    let registration: ManagementRegistration = serde_json::from_slice(&reply.payload)
        .map_err(|_| AdminError::invalid("插件管理声明无效"))?;
    validation::registration(&registration)?;
    let mut resources = BTreeMap::new();
    let mut resource_views = Vec::new();
    let mut total = 0usize;
    for resource in registration.resources {
        let content_type = manifest
            .resources
            .get(&resource.path)
            .filter(|value| validation::content_type(value))
            .ok_or_else(|| AdminError::invalid("管理资源未在包清单声明有效内容类型"))?;
        let bytes = package
            .resource(&resource.path)
            .ok_or_else(|| AdminError::invalid("管理资源不在已校验制品中"))?;
        total = total.saturating_add(bytes.len());
        if bytes.len() > MAXIMUM_BODY_BYTES || total > 8 * MAXIMUM_BODY_BYTES {
            return Err(AdminError::invalid("插件管理资源超过单项或总大小限制"));
        }
        resources.insert(resource.path.clone(), Arc::from(bytes));
        resource_views.push(PluginManagementResource {
            path: resource.path,
            content_type: content_type.clone(),
            public: resource.public,
        });
    }
    for page in &registration.pages {
        if !resource_views.iter().any(|resource| {
            resource.path == page.entry
                && matches!(
                    resource.content_type.as_str(),
                    "text/html" | "text/html; charset=utf-8"
                )
        }) {
            return Err(AdminError::invalid("插件页面入口必须为 HTML 资源"));
        }
    }
    let routes = registration.routes;
    let view = PluginManagementView {
        callbacks: registration
            .callbacks
            .into_iter()
            .map(
                |callback| gateway_admin::model::plugins::management::PluginManagementCallback {
                    path: callback.path,
                    response_content_types: callback.response_content_types,
                },
            )
            .collect(),
        target: PluginManagementTarget {
            instance_id: instance.id.clone(),
            artifact_sha256: instance.artifact_sha256.clone(),
            revision: instance.revision.get(),
        },
        name: instance.name.clone(),
        configuration_schema: manifest.configuration_schema.clone(),
        routes: routes
            .iter()
            .map(|route| PluginManagementRoute {
                method: route.method.clone(),
                path: route.path.clone(),
                request_content_types: route.request_content_types.clone(),
                response_content_types: route.response_content_types.clone(),
            })
            .collect(),
        resources: resource_views,
        pages: registration
            .pages
            .into_iter()
            .map(|page| PluginManagementPage {
                id: page.id,
                title: page.title,
                description: page.description,
                entry: page.entry,
                icon: page.icon,
            })
            .collect(),
    };
    Ok(Some(ManagementEntry {
        view,
        routes,
        resources,
        session,
        callbacks,
    }))
}
