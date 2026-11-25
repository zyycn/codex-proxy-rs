//! 控制面会话入口共同消费的应用服务合同

use gateway_admin::AdminServices;

/// 控制面 HTTP adapter 消费同一组用例；权限由各入口服务端校验
pub trait SessionState {
    fn admin_services(&self) -> &AdminServices;
}
