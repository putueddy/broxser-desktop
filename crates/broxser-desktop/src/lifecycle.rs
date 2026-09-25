//! Transitions of the live runtime. The window runs one Restart or Close at a
//! time and numbers its runtimes, so a late result of a runtime that is stopping
//! (its stop, a wake-up or a decoded frame) cannot act on a newer one (ADR 0009).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// No transition runs. The runtime is running, stopped or failed to start.
    Live,
    /// The previous runtime is stopping; a new one starts afterwards.
    Restarting,
    /// The runtime is stopping or gone for good; nothing starts again.
    Closing,
}

/// Next step once the runtime stopped by a transition has finished stopping.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AfterStop {
    /// Start the runtime of the current generation.
    Start,
    /// Remove the window.
    Close,
}

/// How a close request proceeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CloseRequest {
    /// Nothing runs or stops: the window may close now.
    Now,
    /// Stop the held runtime off the UI thread, then remove the window.
    Stop,
    /// A runtime is already stopping; the window closes after it.
    Wait,
}

#[derive(Debug)]
pub(crate) struct Lifecycle {
    phase: Phase,
    generation: u64,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self {
            phase: Phase::Live,
            generation: 0,
        }
    }
}

impl Lifecycle {
    /// Generation of the runtime whose results the window accepts.
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    /// True if a result of the runtime of `generation` may change the window.
    pub(crate) fn accepts(&self, generation: u64) -> bool {
        self.phase == Phase::Live && generation == self.generation
    }

    /// True while no transition runs, so Restart may be offered.
    pub(crate) fn is_live(&self) -> bool {
        self.phase == Phase::Live
    }

    pub(crate) fn is_restarting(&self) -> bool {
        self.phase == Phase::Restarting
    }

    /// Begins a restart. Returns false, and changes nothing, while another
    /// transition runs: the request is dropped, not queued.
    pub(crate) fn begin_restart(&mut self) -> bool {
        if self.phase != Phase::Live {
            return false;
        }
        self.phase = Phase::Restarting;
        self.generation += 1;
        true
    }

    /// Begins closing. `running` tells whether a runtime is held, which must
    /// stop before the window closes.
    pub(crate) fn begin_close(&mut self, running: bool) -> CloseRequest {
        let request = match self.phase {
            Phase::Closing => return CloseRequest::Wait,
            // The restart's stop continues; its continuation closes the window.
            Phase::Restarting => CloseRequest::Wait,
            Phase::Live if running => CloseRequest::Stop,
            Phase::Live => CloseRequest::Now,
        };
        self.phase = Phase::Closing;
        self.generation += 1;
        request
    }

    /// The runtime stopped by a restart or close has finished stopping.
    pub(crate) fn stopped(&mut self) -> AfterStop {
        debug_assert!(self.phase != Phase::Live, "no transition was stopping");
        match self.phase {
            Phase::Restarting => {
                self.phase = Phase::Live;
                AfterStop::Start
            }
            Phase::Live | Phase::Closing => AfterStop::Close,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AfterStop, CloseRequest, Lifecycle};

    #[test]
    fn a_restart_during_a_restart_changes_nothing() {
        let mut lifecycle = Lifecycle::default();
        assert!(lifecycle.is_live());
        assert!(lifecycle.begin_restart());
        let restarting = lifecycle.generation();
        assert!(!lifecycle.begin_restart(), "a second click is not queued");
        assert_eq!(lifecycle.generation(), restarting);
        assert!(lifecycle.is_restarting() && !lifecycle.is_live());
        assert_eq!(lifecycle.stopped(), AfterStop::Start);
        assert!(lifecycle.is_live());
        // The new runtime may be restarted once it has stopped in turn.
        assert!(lifecycle.begin_restart());
        assert_eq!(lifecycle.stopped(), AfterStop::Start);
    }

    #[test]
    fn close_during_a_restart_starts_nothing() {
        let mut lifecycle = Lifecycle::default();
        assert!(lifecycle.begin_restart());
        assert_eq!(lifecycle.begin_close(false), CloseRequest::Wait);
        assert!(!lifecycle.is_live() && !lifecycle.is_restarting());
        assert!(!lifecycle.begin_restart());
        assert_eq!(lifecycle.stopped(), AfterStop::Close);
        assert!(!lifecycle.accepts(lifecycle.generation()));
        assert!(!lifecycle.begin_restart(), "nothing starts after a close");
    }

    #[test]
    fn close_stops_a_held_runtime_first_and_only_once() {
        let mut lifecycle = Lifecycle::default();
        assert_eq!(lifecycle.begin_close(true), CloseRequest::Stop);
        assert_eq!(lifecycle.begin_close(true), CloseRequest::Wait);
        assert!(!lifecycle.begin_restart());
        assert_eq!(lifecycle.stopped(), AfterStop::Close);
    }

    #[test]
    fn close_without_a_runtime_is_immediate() {
        let mut lifecycle = Lifecycle::default();
        assert_eq!(lifecycle.begin_close(false), CloseRequest::Now);
        assert_eq!(lifecycle.begin_close(false), CloseRequest::Wait);
        assert!(!lifecycle.begin_restart());
    }

    #[test]
    fn results_of_a_replaced_runtime_are_rejected() {
        let mut lifecycle = Lifecycle::default();
        let first = lifecycle.generation();
        assert!(lifecycle.accepts(first));
        assert!(lifecycle.begin_restart());
        // While it stops, the old runtime's frames and wake-ups are rejected,
        // and there is no new runtime yet.
        assert!(!lifecycle.accepts(first));
        assert!(!lifecycle.accepts(lifecycle.generation()));
        assert_eq!(lifecycle.stopped(), AfterStop::Start);
        let second = lifecycle.generation();
        assert_ne!(first, second);
        assert!(lifecycle.accepts(second));
        assert!(!lifecycle.accepts(first));
        assert_eq!(lifecycle.begin_close(true), CloseRequest::Stop);
        assert!(!lifecycle.accepts(second));
    }
}
