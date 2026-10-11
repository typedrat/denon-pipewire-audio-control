//! The daemon's view of the receiver, shared between the telnet session and Varlink clients.

use std::mem;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::protocol::Event;
use crate::scale::{self, Mv};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AvrState {
    pub connected: bool,
    pub power: Option<bool>,
    pub input: Option<String>,
    pub mv: Option<Mv>,
    pub mute: Option<bool>,
    /// The last fraction PipeWire asked for and the level it rounded to. While the receiver
    /// sits at that level the exact fraction is reported, so PipeWire sees its own write
    /// echoed unchanged rather than nudged by half-dB rounding.
    requested: Option<(Mv, f64)>,
}

impl AvrState {
    pub fn volume(&self) -> Option<f64> {
        let mv = self.mv?;
        Some(match self.requested {
            Some((requested, v)) if requested == mv => v,
            _ => scale::from_mv(mv),
        })
    }

    /// Whether writes from PipeWire should reach the receiver.
    pub fn active(&self, pc_input: &str) -> bool {
        self.connected && self.power == Some(true) && self.input.as_deref() == Some(pc_input)
    }
}

/// Holds the receiver state and decides which PipeWire requests become receiver commands.
pub struct Shared {
    tx: watch::Sender<AvrState>,
    pc_input: String,
    cap: Mv,
    /// Writes are refused until this instant.
    hold_until: Mutex<Option<Instant>>,
}

impl Shared {
    pub fn new(pc_input: impl Into<String>, cap: Mv) -> Self {
        Self {
            tx: watch::Sender::new(AvrState::default()),
            pc_input: pc_input.into(),
            cap,
            hold_until: Mutex::new(None),
        }
    }

    /// Refuses volume and mute writes for `d`, exactly as if the receiver were off the PC
    /// input, so stale levels PipeWire pushes right after attaching can't reach the receiver.
    pub fn hold_writes_for(&self, d: Duration) {
        *self.hold_until.lock().expect("hold lock poisoned") = Some(Instant::now() + d);
    }

    fn held(&self) -> bool {
        self.hold_until
            .lock()
            .expect("hold lock poisoned")
            .is_some_and(|deadline| Instant::now() < deadline)
    }

    pub fn cap(&self) -> Mv {
        self.cap
    }

    pub fn subscribe(&self) -> watch::Receiver<AvrState> {
        self.tx.subscribe()
    }

    pub fn snapshot(&self) -> AvrState {
        self.tx.borrow().clone()
    }

    pub fn set_connected(&self, connected: bool) {
        self.tx.send_if_modified(|s| {
            let mut changed = mem::replace(&mut s.connected, connected) != connected;
            if !connected {
                // The next session may find the receiver in another state; until it answers,
                // writes must not be gated on what the last one reported.
                changed |= s.power.take().is_some();
                changed |= s.input.take().is_some();
            }
            changed
        });
    }

    pub fn apply(&self, event: Event) {
        self.tx.send_if_modified(|s| match event {
            Event::Power(on) => s.power.replace(on) != Some(on),
            Event::Input(input) => {
                let changed = s.input.as_deref() != Some(input.as_str());
                s.input = Some(input);
                changed
            }
            Event::Volume(mv) => s.mv.replace(mv) != Some(mv),
            Event::Mute(mute) => s.mute.replace(mute) != Some(mute),
        });
    }

    /// Returns the level to send. `None` means the receiver isn't listening to the PC, and
    /// subscribers are woken so PipeWire snaps back to the receiver's real level.
    pub fn request_volume(&self, v: f64) -> Option<Mv> {
        let mut target = None;
        let held = self.held();
        self.tx.send_if_modified(|s| {
            if held || !s.active(&self.pc_input) {
                return true;
            }
            // Clamp before rounding so that an over-cap request is remembered, and echoed
            // back, as the cap itself. NaN carries no level, so it is treated as silence.
            let max = scale::from_mv(self.cap);
            let clamped = if v.is_nan() { 0.0 } else { v.clamp(0.0, max) };
            let mv = scale::to_mv(clamped, self.cap);
            s.requested = Some((mv, clamped));
            target = Some(mv);
            // Subscribers hear about the new level once the receiver confirms it; notifying
            // now would report the old level and drag PipeWire's slider back mid-change.
            // The exception is a clamped request the receiver already satisfies: it will
            // echo nothing new, so PipeWire's slider would stay at the over-cap value.
            clamped != v && s.mv == Some(mv)
        });
        target
    }

    /// Returns whether to send a mute change, with the same snap-back as `request_volume`.
    pub fn request_mute(&self) -> bool {
        let mut active = false;
        let held = self.held();
        self.tx.send_if_modified(|s| {
            active = !held && s.active(&self.pc_input);
            !active
        });
        active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: Mv = Mv::from_whole(70);

    fn on_pc_input() -> Shared {
        let shared = Shared::new("BD", CAP);
        shared.set_connected(true);
        shared.apply(Event::Power(true));
        shared.apply(Event::Input("BD".into()));
        shared.apply(Event::Volume(Mv::from_whole(62)));
        shared.apply(Event::Mute(false));
        shared
    }

    #[test]
    fn unknown_level_has_no_volume() {
        assert_eq!(Shared::new("BD", CAP).snapshot().volume(), None);
    }

    #[test]
    fn active_needs_connection_power_and_input() {
        let shared = on_pc_input();
        assert!(shared.snapshot().active("BD"));
        shared.apply(Event::Input("GAME".into()));
        assert!(!shared.snapshot().active("BD"));
        shared.apply(Event::Input("BD".into()));
        shared.apply(Event::Power(false));
        assert!(!shared.snapshot().active("BD"));
        shared.apply(Event::Power(true));
        shared.set_connected(false);
        assert!(!shared.snapshot().active("BD"));
    }

    #[test]
    fn confirmed_request_reports_the_exact_fraction() {
        let shared = on_pc_input();
        let mv = shared.request_volume(0.613).expect("active");
        assert_eq!(mv, Mv::from_half_steps(123));
        shared.apply(Event::Volume(mv));
        assert_eq!(shared.snapshot().volume(), Some(0.613));
        shared.apply(Event::Volume(Mv::from_whole(60)));
        assert_eq!(shared.snapshot().volume(), Some(0.60));
    }

    #[test]
    fn requests_above_the_cap_are_clamped() {
        let shared = on_pc_input();
        let mv = shared.request_volume(0.9).expect("active");
        assert_eq!(mv, CAP);
        shared.apply(Event::Volume(mv));
        assert_eq!(shared.snapshot().volume(), Some(0.70));
    }

    #[test]
    fn request_waits_for_confirmation_before_notifying() {
        let shared = on_pc_input();
        let mut rx = shared.subscribe();
        rx.borrow_and_update();
        let mv = shared.request_volume(0.613).expect("active");
        assert!(!rx.has_changed().unwrap());
        shared.apply(Event::Volume(mv));
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn inactive_requests_are_refused_and_republished() {
        let shared = on_pc_input();
        shared.apply(Event::Input("GAME".into()));
        let mut rx = shared.subscribe();
        rx.borrow_and_update();
        assert_eq!(shared.request_volume(0.5), None);
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        assert!(!shared.request_mute());
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn active_mute_request_is_sent_without_notifying() {
        let shared = on_pc_input();
        let mut rx = shared.subscribe();
        rx.borrow_and_update();
        assert!(shared.request_mute());
        assert!(!rx.has_changed().unwrap());
    }

    #[test]
    fn writes_are_refused_during_a_hold_and_accepted_after() {
        let shared = on_pc_input();
        shared.hold_writes_for(Duration::from_millis(50));
        let mut rx = shared.subscribe();
        rx.borrow_and_update();
        assert_eq!(shared.request_volume(0.5), None);
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        assert!(!shared.request_mute());
        assert!(rx.has_changed().unwrap());

        std::thread::sleep(Duration::from_millis(80));
        assert!(shared.request_volume(0.5).is_some());
        assert!(shared.request_mute());
    }

    #[test]
    fn over_cap_write_at_the_cap_snaps_back() {
        let shared = on_pc_input();
        shared.apply(Event::Volume(CAP));
        let mut rx = shared.subscribe();
        rx.borrow_and_update();
        assert_eq!(shared.request_volume(0.75), Some(CAP));
        assert!(rx.has_changed().unwrap());
        assert_eq!(shared.snapshot().volume(), Some(0.70));
    }

    #[test]
    fn disconnect_forgets_power_and_input_but_not_level() {
        let shared = on_pc_input();
        shared.set_connected(false);
        let s = shared.snapshot();
        assert_eq!((s.power, s.input), (None, None));
        assert_eq!(s.mv, Some(Mv::from_whole(62)));
        assert_eq!(s.mute, Some(false));
        shared.set_connected(true);
        assert!(!shared.snapshot().active("BD"));
        assert_eq!(shared.request_volume(0.5), None);
    }

    #[test]
    fn nan_and_negative_requests_are_silence() {
        let shared = on_pc_input();
        assert_eq!(shared.request_volume(f64::NAN), Some(Mv::ZERO));
        assert_eq!(shared.request_volume(-0.5), Some(Mv::ZERO));
    }

    #[test]
    fn levels_above_the_cap_read_back_as_they_are() {
        let shared = on_pc_input();
        shared.apply(Event::Volume(Mv::from_whole(75)));
        assert_eq!(shared.snapshot().volume(), Some(0.75));
    }

    #[test]
    fn real_changes_notify() {
        let shared = on_pc_input();
        let mut rx = shared.subscribe();
        rx.borrow_and_update();
        shared.apply(Event::Volume(Mv::from_whole(50)));
        assert!(rx.has_changed().unwrap());
        rx.borrow_and_update();
        shared.set_connected(false);
        assert!(rx.has_changed().unwrap());
    }

    #[test]
    fn repeated_events_do_not_notify() {
        let shared = on_pc_input();
        let mut rx = shared.subscribe();
        rx.borrow_and_update();
        shared.apply(Event::Volume(Mv::from_whole(62)));
        shared.apply(Event::Power(true));
        shared.apply(Event::Input("BD".into()));
        shared.set_connected(true);
        assert!(!rx.has_changed().unwrap());
    }
}
