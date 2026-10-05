//! 与协议无关的洋葱组合器；输入、结果与错误保留调用方的领域类型

pub mod http;
pub mod service;
pub mod websocket;

use futures::future::BoxFuture;

/// 单次调用中的一层；上下文放在类型化输入中或由闭包捕获
pub type Middleware<Input, Output, Error> = Box<
    dyn FnOnce(Input, Next<Input, Output, Error>) -> BoxFuture<'static, Result<Output, Error>>
        + Send,
>;

type Terminal<Input, Output, Error> =
    Box<dyn FnOnce(Input) -> BoxFuture<'static, Result<Output, Error>> + Send>;

/// 消费自身后进入下一层，所有权保证同一个续体不能运行两次
///
/// 返回流或会话时，资源寿命由结果对象持有，组合器不后台驱动或缓冲结果
pub struct Next<Input, Output, Error> {
    layers: std::vec::IntoIter<Middleware<Input, Output, Error>>,
    terminal: Terminal<Input, Output, Error>,
}

impl<Input, Output, Error> Next<Input, Output, Error> {
    /// 直接返回下一层 future，不启动与父调用脱离的任务
    pub fn run(mut self, input: Input) -> BoxFuture<'static, Result<Output, Error>> {
        match self.layers.next() {
            Some(layer) => layer(input, self),
            None => (self.terminal)(input),
        }
    }
}

/// 按给定顺序进入中间件，响应与错误沿等待栈逆序返回
///
/// 空链直接调用终端；输入不需要 Clone，不经过 JSON 或字符串操作分派
pub fn compose<Input, Output, Error>(
    layers: Vec<Middleware<Input, Output, Error>>,
    terminal: impl FnOnce(Input) -> BoxFuture<'static, Result<Output, Error>> + Send + 'static,
) -> Next<Input, Output, Error> {
    Next {
        layers: layers.into_iter(),
        terminal: Box::new(terminal),
    }
}
