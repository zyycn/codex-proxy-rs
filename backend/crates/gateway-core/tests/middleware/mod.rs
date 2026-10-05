//! 验证中间件嵌套调用顺序、类型传递与短路后的资源释放

use std::sync::{Arc, Mutex};

use gateway_core::middleware::{Middleware, compose};

#[test]
fn onion_preserves_typed_input_and_reverse_return_order() {
    futures::executor::block_on(async {
        let events = Arc::new(Mutex::new(Vec::new()));
        let layers = (1..=3)
            .map(|index| -> Middleware<Vec<u8>, Vec<u8>, &'static str> {
                let events = events.clone();
                Box::new(move |mut input, next| {
                    Box::pin(async move {
                        events.lock().unwrap().push(index);
                        input.push(index);
                        let mut output = next.run(input).await?;
                        events.lock().unwrap().push(10 + index);
                        output.push(10 + index);
                        Ok(output)
                    })
                })
            })
            .collect();
        let output = compose(layers, |input| Box::pin(async { Ok(input) }))
            .run(vec![0])
            .await
            .unwrap();
        assert_eq!(output, [0, 1, 2, 3, 13, 12, 11]);
        assert_eq!(*events.lock().unwrap(), [1, 2, 3, 13, 12, 11]);
    });
}

#[test]
fn short_circuit_drops_unconsumed_terminal_and_recovers_outer_error() {
    futures::executor::block_on(async {
        let resource = Arc::new(());
        let retained = resource.clone();
        let layers: Vec<Middleware<(), u32, &'static str>> = vec![
            Box::new(|input, next| {
                Box::pin(async move {
                    assert_eq!(next.run(input).await, Err("rejected"));
                    Ok(204)
                })
            }),
            Box::new(|_, _next| Box::pin(async { Err("rejected") })),
        ];
        let result = compose(layers, move |()| {
            drop(retained);
            panic!("短路不能调用终端")
        })
        .run(())
        .await;
        assert_eq!(result, Ok(204));
        assert_eq!(Arc::strong_count(&resource), 1);
    });
}
