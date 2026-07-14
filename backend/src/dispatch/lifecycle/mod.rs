//! `v1/*` 请求的生命周期基础类型与入口准备。

pub(in crate::dispatch) mod attempt;
pub(in crate::dispatch) mod contract;
pub(in crate::dispatch) mod finalizer;
pub(in crate::dispatch) mod pipeline;
pub(in crate::dispatch) mod request;
pub(in crate::dispatch) mod stream;
pub(in crate::dispatch) mod trace;

pub(in crate::dispatch) use request::{
    RequestContext, RequestEnterDependencies, RequestMode, enter_request,
};
