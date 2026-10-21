use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Intent {
    Manual,
    Playback,
    Recovery,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Decision {
    Seek(usize),
    Wait,
    Stop,
}

struct Request {
    frame: usize,
    intent: Intent,
    started: Instant,
    first: usize,
    last: usize,
    retained: bool,
}

#[derive(Default)]
pub(super) struct Recovery {
    request: Option<Request>,
    expired_loads: u32,
    frames_per_second: f64,
    load_time: Duration,
    awaiting_playback_step: bool,
    waiting_since: Option<Instant>,
}

impl Recovery {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn make_manual(&mut self) {
        self.expired_loads = 0;
        self.awaiting_playback_step = false;
        self.waiting_since = None;
        if let Some(request) = &mut self.request {
            request.intent = Intent::Manual;
        }
    }

    pub fn begin(
        &mut self,
        intent: Intent,
        frame: usize,
        range: (usize, usize),
        retained: bool,
        now: Instant,
    ) {
        if intent == Intent::Manual {
            self.clear();
        }
        self.waiting_since = None;
        self.request = Some(Request {
            frame,
            intent,
            started: now,
            first: range.0,
            last: range.1,
            retained,
        });
    }

    pub fn finish(
        &mut self,
        frame: usize,
        range: (usize, usize),
        retained: bool,
        now: Instant,
    ) -> Intent {
        let Some(request) = self.request.take().filter(|request| request.frame == frame) else {
            return Intent::Manual;
        };
        if request.intent != Intent::Manual {
            let elapsed = now.saturating_duration_since(request.started);
            let advance = range
                .0
                .saturating_sub(request.first)
                .max(range.1.saturating_sub(request.last));
            let rate = advance as f64 / elapsed.as_secs_f64().max(0.001);
            self.frames_per_second = rate.max(self.frames_per_second * 0.75);
            let recovering = request.intent == Intent::Recovery || self.awaiting_playback_step;
            if request.intent == Intent::Recovery {
                self.load_time = elapsed;
            }
            if retained && request.intent == Intent::Playback {
                self.expired_loads = 0;
                self.awaiting_playback_step = false;
            } else if retained {
                self.awaiting_playback_step = true;
            } else if recovering && (request.retained || self.awaiting_playback_step) {
                self.expired_loads = self.expired_loads.saturating_add(1);
                self.load_time = self.load_time.max(elapsed);
                self.awaiting_playback_step = false;
            }
        }
        request.intent
    }

    pub fn target(&mut self, first: usize, last: usize, connected: bool) -> Decision {
        if self.awaiting_playback_step {
            self.expired_loads = self.expired_loads.saturating_add(1);
            self.awaiting_playback_step = false;
        }
        if !connected || self.expired_loads == 0 {
            return Decision::Seek(first);
        }
        if self.expired_loads >= 3 || first >= last {
            return Decision::Stop;
        }
        let during_load = (self.frames_per_second * self.load_time.as_secs_f64()).ceil() as usize;
        let multiplier = 1usize << self.expired_loads;
        let margin = during_load.saturating_add(1).saturating_mul(multiplier);
        // Keep one subsequent history frame available instead of selecting the live edge.
        let margin = margin.min(last.saturating_sub(first).saturating_sub(1));
        Decision::Seek(first.saturating_add(margin))
    }

    pub fn unavailable(&mut self, connected: bool, now: Instant) -> Decision {
        let since = *self.waiting_since.get_or_insert(now);
        if !connected || now.saturating_duration_since(since) >= Duration::from_secs(2) {
            Decision::Stop
        } else {
            Decision::Wait
        }
    }

    pub fn waiting(&self) -> bool {
        self.waiting_since.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_first_attempt_then_margin_tracks_expiration_during_loading() {
        let now = Instant::now();
        let mut recovery = Recovery::default();
        assert_eq!(recovery.target(100, 400, true), Decision::Seek(100));
        recovery.begin(Intent::Recovery, 100, (100, 400), true, now);
        assert_eq!(
            recovery.finish(100, (132, 432), false, now + Duration::from_secs(1)),
            Intent::Recovery
        );
        assert_eq!(recovery.target(132, 432, true), Decision::Seek(198));
        recovery.begin(
            Intent::Recovery,
            198,
            (132, 432),
            true,
            now + Duration::from_secs(1),
        );
        recovery.finish(198, (164, 464), true, now + Duration::from_secs(2));
        recovery.begin(
            Intent::Playback,
            199,
            (164, 464),
            true,
            now + Duration::from_secs(2),
        );
        recovery.finish(199, (165, 465), true, now + Duration::from_millis(2016));
        assert_eq!(recovery.target(164, 464, true), Decision::Seek(164));
    }

    #[test]
    fn repeated_expiration_is_bounded_and_never_forces_live_edge() {
        let now = Instant::now();
        let mut recovery = Recovery::default();
        for attempt in 0..3 {
            let first = 100 + attempt * 100;
            recovery.begin(Intent::Recovery, first, (first, first + 2), true, now);
            recovery.finish(
                first,
                (first + 100, first + 102),
                false,
                now + Duration::from_secs(1),
            );
            assert_eq!(
                recovery.target(first + 100, first + 102, true),
                if attempt == 2 {
                    Decision::Stop
                } else {
                    Decision::Seek(first + 101)
                }
            );
        }
        assert_eq!(recovery.target(400, 400, true), Decision::Stop);
        assert_eq!(recovery.target(400, 402, false), Decision::Seek(400));
    }

    #[test]
    fn measured_seven_rebase_sequence_reaches_a_retained_target() {
        let now = Instant::now();
        let mut recovery = Recovery::default();
        let first = 11_584;
        assert_eq!(
            recovery.target(first, first + 250, true),
            Decision::Seek(first)
        );
        recovery.begin(Intent::Recovery, first, (first, first + 250), true, now);
        recovery.finish(first, (11_646, 11_896), false, now + Duration::from_secs(1));
        let Decision::Seek(target) = recovery.target(11_646, 11_896, true) else {
            panic!("a 250-frame window must allow a bounded recovery margin");
        };
        assert_eq!(target, 11_772);
        recovery.begin(Intent::Recovery, target, (11_646, 11_896), true, now);
        recovery.finish(
            target,
            (11_649, 11_899),
            true,
            now + Duration::from_millis(772),
        );
        recovery.begin(
            Intent::Playback,
            target + 1,
            (11_649, 11_899),
            true,
            now + Duration::from_millis(772),
        );
        recovery.finish(
            target + 1,
            (11_649, 11_899),
            true,
            now + Duration::from_millis(788),
        );
        assert_eq!(
            recovery.target(11_649, 11_899, true),
            Decision::Seek(11_649)
        );
    }

    #[test]
    fn ordinary_playback_expiration_still_attempts_the_exact_first_frame() {
        let now = Instant::now();
        let mut recovery = Recovery::default();
        recovery.begin(Intent::Playback, 100, (100, 400), true, now);
        recovery.finish(100, (150, 450), false, now + Duration::from_secs(1));
        assert_eq!(recovery.target(150, 450, true), Decision::Seek(150));
    }

    #[test]
    fn retained_recovery_frame_must_allow_one_playback_step_before_retries_reset() {
        let now = Instant::now();
        let mut recovery = Recovery::default();
        for attempt in 0..3 {
            let first = 100 + attempt * 100;
            recovery.begin(Intent::Recovery, first, (first, first + 250), true, now);
            recovery.finish(
                first,
                (first, first + 250),
                true,
                now + Duration::from_secs(1),
            );
            recovery.begin(
                Intent::Playback,
                first + 1,
                (first, first + 250),
                true,
                now + Duration::from_secs(1),
            );
            recovery.finish(
                first + 1,
                (first + 100, first + 350),
                false,
                now + Duration::from_secs(2),
            );
            let decision = recovery.target(first + 100, first + 350, true);
            if attempt < 2 {
                assert!(
                    matches!(decision, Decision::Seek(target) if target > first + 100 && target < first + 350)
                );
            } else {
                assert_eq!(decision, Decision::Stop);
            }
        }
    }

    #[test]
    fn retention_advancing_before_the_next_request_keeps_the_same_recovery_budget() {
        let now = Instant::now();
        let mut recovery = Recovery::default();
        for attempt in 0..3 {
            let first = 100 + attempt * 100;
            recovery.begin(Intent::Recovery, first, (first, first + 250), true, now);
            recovery.finish(
                first,
                (first, first + 250),
                true,
                now + Duration::from_secs(1),
            );
            let decision = recovery.target(first + 100, first + 350, true);
            if attempt < 2 {
                assert!(
                    matches!(decision, Decision::Seek(target) if target > first + 100 && target < first + 350)
                );
            } else {
                assert_eq!(decision, Decision::Stop);
            }
        }
    }

    #[test]
    fn manual_requests_cancellation_and_stale_metadata_do_not_accumulate_retries() {
        let now = Instant::now();
        let mut recovery = Recovery::default();
        recovery.begin(Intent::Recovery, 100, (100, 400), true, now);
        recovery.make_manual();
        assert_eq!(
            recovery.finish(100, (200, 500), false, now + Duration::from_secs(1)),
            Intent::Manual
        );
        assert_eq!(recovery.target(200, 500, true), Decision::Seek(200));
        for _ in 0..10 {
            recovery.begin(Intent::Recovery, 200, (200, 500), false, now);
            recovery.finish(200, (200, 500), false, now + Duration::from_millis(1));
        }
        assert_eq!(recovery.target(200, 500, true), Decision::Seek(200));
        assert_eq!(recovery.unavailable(true, now), Decision::Wait);
        assert_eq!(
            recovery.unavailable(true, now + Duration::from_secs(2)),
            Decision::Stop
        );
        recovery.clear();
        assert!(!recovery.waiting());
        assert_eq!(recovery.target(100, 400, true), Decision::Seek(100));
    }
}
