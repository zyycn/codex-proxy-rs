//! 记录真实插件派生 HTTP 调用的入口快照与递归作用域

use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use gateway_core::engine::{
    extensions::RequestContext,
    middleware::{MiddlewareError, MiddlewareHeader, http},
};

pub fn request() -> Arc<RequestContext> {
    Arc::new(RequestContext {
        method: "POST".into(),
        uri: "/entry?fixture=original".into(),
        http_version: "HTTP/2".into(),
        headers: vec![
            MiddlewareHeader::new("x-repeat", "first".into()),
            MiddlewareHeader::new("x-repeat", vec![128, 255].into()),
        ],
        peer_addr: Some("[2001:db8::1]:1234".parse().unwrap()),
        client_ip: Some("203.0.113.7".parse().unwrap()),
    })
}

#[derive(Default)]
pub struct Dispatcher(pub Mutex<Vec<http::Context>>);

impl http::Dispatcher for Dispatcher {
    fn dispatch(
        &self,
        context: http::Context,
        request: http::Request,
    ) -> BoxFuture<'static, Result<http::Response, MiddlewareError>> {
        assert_eq!(request.uri(), "/derived");
        self.0.lock().unwrap().push(context);
        Box::pin(async { Ok(http::Response::new(http::empty_body())) })
    }
}
