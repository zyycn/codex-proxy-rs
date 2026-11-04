-- 预算窗口和费用事件归属请求使用的 Key ID，凭据删除后仍可结算在途请求
alter table client_api_key_budget_windows
    drop constraint client_api_key_budget_windows_client_api_key_id_fkey;
alter table client_api_key_charge_events
    drop constraint client_api_key_charge_events_client_api_key_id_fkey;
