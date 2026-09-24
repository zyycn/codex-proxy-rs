-- 配置恢复与插件私有状态迁移分离，删除实例或制品时同步清理对应快照。
create table plugin_version_configurations (
    instance_id uuid not null references plugin_instances(id) on delete cascade,
    artifact_sha256 text not null references plugin_artifacts(sha256) on delete cascade,
    configuration_json jsonb not null check (jsonb_typeof(configuration_json) = 'object'),
    secrets_json jsonb not null check (jsonb_typeof(secrets_json) = 'object'),
    bindings_json jsonb not null check (jsonb_typeof(bindings_json) = 'array'),
    primary key (instance_id, artifact_sha256)
);

-- 只为已有启用配置建立恢复点，未启用草稿不能冒充可用版本。
insert into plugin_version_configurations
    (instance_id, artifact_sha256, configuration_json, secrets_json, bindings_json)
select i.id, i.artifact_sha256, i.configuration_json,
       coalesce(s.secrets_json, '{}'::jsonb), i.bindings_json
from plugin_instances i
left join plugin_instance_secrets s on s.instance_id = i.id
where i.enabled;
