use super::*;

#[test]
fn waiting_for_registry_checks_cancellation_and_allows_retry() {
    let state = Mutex::new(7);
    let held = state.lock().unwrap();
    let mut checks = 0;
    let result = lock_with_cancel(&state, &mut || {
        checks += 1;
        checks == 3
    });
    assert!(result.is_err());
    assert_eq!(checks, 3);
    assert_eq!(*held, 7);
    drop(held);
    assert_eq!(*lock_with_cancel(&state, &mut || false).unwrap(), 7);
}

#[test]
fn cancelled_registry_request_does_not_acquire_an_available_lock() {
    let state = Mutex::new(7);
    assert!(lock_with_cancel(&state, &mut || true).is_err());
    assert_eq!(*state.try_lock().unwrap(), 7);
}
