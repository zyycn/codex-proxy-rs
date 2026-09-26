use futures::FutureExt;
use gateway_core::engine::response_control::{ResponseControl, ResponseInterruptError};

#[test]
fn interrupt_requires_the_current_response_owner_and_does_not_extend_its_lifetime() {
    let control = ResponseControl::default();
    assert_eq!(
        control.interrupt("resp_a"),
        Err(ResponseInterruptError::Unavailable)
    );
    assert!(control.activate(String::new()).is_none());
    let active = control.activate("resp_a".to_owned()).unwrap();
    let requested = active.requested();
    assert!(control.activate("resp_b".to_owned()).is_none());
    assert_eq!(
        control.interrupt("resp_b"),
        Err(ResponseInterruptError::ResponseMismatch)
    );
    assert!(active.requested().now_or_never().is_none());
    control.interrupt("resp_a").unwrap();
    control.interrupt("resp_a").unwrap();
    assert_eq!(requested.now_or_never(), Some(()));
    let retained_waiter = active.requested();
    drop(active);
    assert_eq!(
        control.interrupt("resp_a"),
        Err(ResponseInterruptError::Unavailable)
    );
    let next = control.activate("resp_b".to_owned()).unwrap();
    assert!(next.requested().now_or_never().is_none());
    drop(retained_waiter);
}

#[test]
fn matching_response_ids_do_not_share_control_between_root_executions() {
    let first = ResponseControl::default();
    let second = ResponseControl::default();
    let active_first = first.activate("resp_same".to_owned()).unwrap();
    let active_second = second.activate("resp_same".to_owned()).unwrap();
    first.interrupt("resp_same").unwrap();
    assert_eq!(active_first.requested().now_or_never(), Some(()));
    assert!(active_second.requested().now_or_never().is_none());
}
