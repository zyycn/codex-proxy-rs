-- 删除 Key 时按稳定身份定位费用事件，避免扫描其他 Key 的完整账本
create index client_key_charge_events_client_api_key_id
    on client_key_charge_events (client_api_key_id);
