-- 插件清单只声明处理器与配置；制品接受记录保留用户的完整信任决定。
update plugin_artifacts
set metadata_json = metadata_json - array['requestedPermissions', 'permissionDescriptions']
where metadata_json ?| array['requestedPermissions', 'permissionDescriptions'];
