use super::ReceiverEvent;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{
    self, Receiver, RecvError, RecvTimeoutError, SyncSender, TryRecvError, TrySendError,
};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[derive(Default)]
pub(super) struct DeliverySignal {
    // Queue checks, dequeue notifications, and cancellation share this lock to prevent lost wakeups.
    gate: Mutex<()>,
    space_available: Condvar,
}

impl DeliverySignal {
    pub(super) fn stop(&self, cancel: &AtomicBool) {
        let _guard = self.gate.lock().unwrap();
        cancel.store(true, Ordering::Release);
        self.space_available.notify_all();
    }

    fn notify(&self) {
        let _guard = self.gate.lock().unwrap();
        self.space_available.notify_one();
    }
}

pub struct EventReceiver {
    receiver: Option<Receiver<ReceiverEvent>>,
    signal: Arc<DeliverySignal>,
}

impl EventReceiver {
    pub fn recv(&self) -> Result<ReceiverEvent, RecvError> {
        self.received(self.receiver.as_ref().unwrap().recv())
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<ReceiverEvent, RecvTimeoutError> {
        self.received(self.receiver.as_ref().unwrap().recv_timeout(timeout))
    }

    pub fn try_recv(&self) -> Result<ReceiverEvent, TryRecvError> {
        self.received(self.receiver.as_ref().unwrap().try_recv())
    }

    fn received<E>(&self, result: Result<ReceiverEvent, E>) -> Result<ReceiverEvent, E> {
        if result.is_ok() {
            self.signal.notify();
        }
        result
    }
}

impl Drop for EventReceiver {
    fn drop(&mut self) {
        drop(self.receiver.take());
        self.signal.notify();
    }
}

pub(super) struct Output {
    sender: SyncSender<ReceiverEvent>,
    pub(super) cancel: Arc<AtomicBool>,
    pub(super) signal: Arc<DeliverySignal>,
    wake: Box<dyn Fn() + Send + Sync>,
}

impl Output {
    pub(super) fn channel(
        capacity: usize,
        cancel: Arc<AtomicBool>,
        wake: impl Fn() + Send + Sync + 'static,
    ) -> (Self, EventReceiver) {
        let (sender, receiver) = mpsc::sync_channel(capacity);
        let signal = Arc::new(DeliverySignal::default());
        (
            Self {
                sender,
                cancel,
                signal: signal.clone(),
                wake: Box::new(wake),
            },
            EventReceiver {
                receiver: Some(receiver),
                signal,
            },
        )
    }

    pub(super) fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }

    pub(super) fn send(&self, mut event: ReceiverEvent) -> bool {
        let mut guard = self.signal.gate.lock().unwrap();
        while !self.stopped() {
            match self.sender.try_send(event) {
                Ok(()) => {
                    drop(guard);
                    (self.wake)();
                    return true;
                }
                Err(TrySendError::Full(pending)) => {
                    event = pending;
                    guard = self.signal.space_available.wait(guard).unwrap();
                }
                Err(TrySendError::Disconnected(_)) => return false,
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use std::time::Instant;

    fn event(byte: u8) -> ReceiverEvent {
        ReceiverEvent::Data {
            session: 3,
            bytes: vec![byte],
        }
    }

    fn value(event: ReceiverEvent) -> u8 {
        let ReceiverEvent::Data { session: 3, bytes } = event else {
            panic!("unexpected event: {event:?}");
        };
        assert_eq!(bytes.len(), 1);
        bytes[0]
    }

    #[test]
    fn saturated_queue_preserves_order_across_all_receive_methods() {
        for mode in 0..3 {
            let (output, receiver) = Output::channel(1, Arc::default(), || {});
            assert!(output.send(event(0)));
            let (attempting, attempted) = mpsc::channel();
            let (completed, completion) = mpsc::channel();
            let producer = thread::spawn(move || {
                for byte in 1..=100 {
                    attempting.send(byte).unwrap();
                    assert!(output.send(event(byte)));
                    completed.send(byte).unwrap();
                }
            });
            for byte in 1..=100 {
                assert_eq!(
                    attempted.recv_timeout(Duration::from_secs(3)).unwrap(),
                    byte
                );
                assert_eq!(completion.try_recv(), Err(TryRecvError::Empty));
                let received = match mode {
                    0 => receiver.recv().unwrap(),
                    1 => receiver.recv_timeout(Duration::from_secs(3)).unwrap(),
                    _ => receiver.try_recv().unwrap(),
                };
                assert_eq!(value(received), byte - 1);
                assert_eq!(
                    completion.recv_timeout(Duration::from_secs(3)).unwrap(),
                    byte
                );
            }
            assert_eq!(value(receiver.recv().unwrap()), 100);
            producer.join().unwrap();
            assert!(matches!(receiver.recv(), Err(RecvError)));
            assert!(matches!(
                receiver.try_recv(),
                Err(TryRecvError::Disconnected)
            ));
            assert!(matches!(
                receiver.recv_timeout(Duration::from_secs(3)),
                Err(RecvTimeoutError::Disconnected)
            ));
        }
    }

    #[test]
    fn dequeue_notification_cannot_precede_the_full_queue_wait() {
        let (output, receiver) = Output::channel(1, Arc::default(), || {});
        assert!(output.send(event(7)));
        let signal = output.signal.clone();
        let guard = signal.gate.lock().unwrap();
        let (attempting, attempted) = mpsc::channel();
        let consumer = thread::spawn(move || {
            attempting.send(()).unwrap();
            assert_eq!(value(receiver.recv().unwrap()), 7);
        });
        attempted.recv_timeout(Duration::from_secs(3)).unwrap();
        let (guard, timeout) = signal
            .space_available
            .wait_timeout(guard, Duration::from_secs(3))
            .unwrap();
        assert!(
            !timeout.timed_out(),
            "dequeue must signal the waiting sender"
        );
        drop(guard);
        consumer.join().unwrap();
        assert!(!output.send(event(8)));
    }

    #[test]
    fn cancelling_saturated_delivery_wakes_sender_and_keeps_buffered_prefix() {
        for _ in 0..32 {
            let cancel = Arc::<AtomicBool>::default();
            let (output, receiver) = Output::channel(1, cancel.clone(), || {});
            let signal = output.signal.clone();
            assert!(output.send(event(1)));
            let (attempting, attempted) = mpsc::channel();
            let (completed, completion) = mpsc::channel();
            let producer = thread::spawn(move || {
                attempting.send(()).unwrap();
                completed.send(output.send(event(2))).unwrap();
            });
            attempted.recv_timeout(Duration::from_secs(3)).unwrap();
            signal.stop(&cancel);
            assert!(!completion.recv_timeout(Duration::from_secs(3)).unwrap());
            producer.join().unwrap();
            assert_eq!(value(receiver.recv().unwrap()), 1);
            assert!(matches!(receiver.recv(), Err(RecvError)));
        }
    }

    #[test]
    fn dropping_receiver_wakes_a_saturated_sender() {
        for _ in 0..32 {
            let (output, receiver) = Output::channel(1, Arc::default(), || {});
            assert!(output.send(event(1)));
            let (attempting, attempted) = mpsc::channel();
            let (completed, completion) = mpsc::channel();
            let producer = thread::spawn(move || {
                attempting.send(()).unwrap();
                completed.send(output.send(event(2))).unwrap();
            });
            attempted.recv_timeout(Duration::from_secs(3)).unwrap();
            drop(receiver);
            assert!(!completion.recv_timeout(Duration::from_secs(3)).unwrap());
            producer.join().unwrap();
        }
    }

    #[test]
    fn successful_send_releases_delivery_lock_before_callback() {
        let receiver_slot = Arc::new(Mutex::new(None::<EventReceiver>));
        let slot = receiver_slot.clone();
        let (output, receiver) = Output::channel(1, Arc::default(), move || {
            assert_eq!(
                value(slot.lock().unwrap().as_ref().unwrap().recv().unwrap()),
                4
            );
        });
        *receiver_slot.lock().unwrap() = Some(receiver);
        assert!(output.send(event(4)));
        assert!(matches!(
            receiver_slot.lock().unwrap().as_ref().unwrap().try_recv(),
            Err(TryRecvError::Empty)
        ));
    }

    #[test]
    fn empty_delivery_wait_times_out_without_disconnect() {
        let (output, receiver) = Output::channel(1, Arc::default(), || {});
        assert!(matches!(
            receiver.recv_timeout(Duration::from_millis(1)),
            Err(RecvTimeoutError::Timeout)
        ));
        assert!(output.send(event(5)));
        assert_eq!(value(receiver.recv_timeout(Duration::ZERO).unwrap()), 5);
    }

    #[test]
    #[ignore = "release-mode saturated queue latency measurement"]
    fn saturated_delivery_wakeup_latency() {
        let (output, receiver) = Output::channel(1, Arc::default(), || {});
        assert!(output.send(event(0)));
        let (attempting, attempted) = mpsc::channel();
        let (completed, completion) = mpsc::channel();
        let producer = thread::spawn(move || {
            for byte in 1..=100 {
                attempting.send(byte).unwrap();
                assert!(output.send(event(byte)));
                completed.send(byte).unwrap();
            }
        });
        let mut latencies = Vec::new();
        for byte in 1..=100 {
            assert_eq!(
                attempted.recv_timeout(Duration::from_secs(3)).unwrap(),
                byte
            );
            thread::sleep(Duration::from_millis(2));
            assert_eq!(completion.try_recv(), Err(TryRecvError::Empty));
            let started = Instant::now();
            assert_eq!(value(receiver.recv().unwrap()), byte - 1);
            assert_eq!(
                completion.recv_timeout(Duration::from_secs(3)).unwrap(),
                byte
            );
            latencies.push(started.elapsed().as_nanos());
        }
        assert_eq!(value(receiver.recv().unwrap()), 100);
        producer.join().unwrap();
        latencies.sort_unstable();
        println!(
            "saturated_delivery samples={} min_ns={} median_ns={} p95_ns={} max_ns={}",
            latencies.len(),
            latencies[0],
            latencies[50],
            latencies[95],
            latencies[99]
        );
    }
}
