use codex_proxy_rs::admin::api::accounts::{
    cookies::{AccountCookiesData, SetAccountCookiesRequest},
    create::CreateAccountRequest,
    delete::{BatchDeleteAccountsRequest, DeleteAccountData},
    export::AccountExportQuery,
    health::{HealthCheckData, HealthCheckRequest},
    import::{AccountImportData, ImportCliAuthRequest},
    lifecycle::{
        BatchUpdateAccountStatusRequest, ResetAccountUsageData, UpdateAccountLabelRequest,
        UpdateAccountStatusRequest,
    },
    list::AccountsQuery,
    oauth::{
        auth_callback, auth_code_relay, auth_device_login, auth_device_poll, auth_login_start,
        AdminAuthCallbackQuery, AdminAuthCodeRelayData, AdminAuthCodeRelayRequest,
        AdminAuthDeviceLoginData, AdminAuthDevicePollData, AdminAuthLoginStartData,
    },
    quota::{AccountQuotaData, AccountQuotaWarningsData},
};

#[test]
fn admin_accounts_handlers_are_split_by_workflow() {
    let _list_query = std::any::type_name::<AccountsQuery>();
    let _export_query = std::any::type_name::<AccountExportQuery>();
    let _create_request = std::any::type_name::<CreateAccountRequest>();
    let _import_data = std::any::type_name::<AccountImportData>();
    let _cli_request = std::any::type_name::<ImportCliAuthRequest>();
    let _label_request = std::any::type_name::<UpdateAccountLabelRequest>();
    let _status_request = std::any::type_name::<UpdateAccountStatusRequest>();
    let _reset_data = std::any::type_name::<ResetAccountUsageData>();
    let _batch_status = std::any::type_name::<BatchUpdateAccountStatusRequest>();
    let _delete_data = std::any::type_name::<DeleteAccountData>();
    let _batch_delete = std::any::type_name::<BatchDeleteAccountsRequest>();
    let _health_request = std::any::type_name::<HealthCheckRequest>();
    let _health_data = std::any::type_name::<HealthCheckData>();
    let _quota_data = std::any::type_name::<AccountQuotaData>();
    let _quota_warnings = std::any::type_name::<AccountQuotaWarningsData>();
    let _set_cookies = std::any::type_name::<SetAccountCookiesRequest>();
    let _cookies_data = std::any::type_name::<AccountCookiesData>();
    let _device_login = std::any::type_name::<AdminAuthDeviceLoginData>();
    let _device_poll = std::any::type_name::<AdminAuthDevicePollData>();
    let _login_start = std::any::type_name::<AdminAuthLoginStartData>();
    let _code_relay = std::any::type_name::<AdminAuthCodeRelayRequest>();
    let _callback_query = std::any::type_name::<AdminAuthCallbackQuery>();
    let _code_relay_data = std::any::type_name::<AdminAuthCodeRelayData>();
    let _device_login_handler = auth_device_login;
    let _device_poll_handler = auth_device_poll;
    let _login_start_handler = auth_login_start;
    let _code_relay_handler = auth_code_relay;
    let _callback_handler = auth_callback;
}
