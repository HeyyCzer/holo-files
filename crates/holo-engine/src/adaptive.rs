//! Adaptive parallelism for the op executor.
//!
//! Every tick the controller picks how many ops may run at once from:
//! - demand: never more workers than queued ops;
//! - how the PC is being used: in use → half, in use with busy CPU → minimum,
//!   idle for a while → maximum, on battery → half of that;
//! - the service: on throttling (HTTP 429/503) halve and hold for a while.
//!
//! Growth is "slow start" (doubling) until the first throttle, then +1 per tick,
//! like TCP congestion control.

use std::time::{Duration, Instant};

/// How many journal ops run at the same time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Concurrency {
    Fixed(usize),
    Auto { min: usize, max: usize },
}

impl Default for Concurrency {
    fn default() -> Self {
        Self::Auto { min: 1, max: 16 }
    }
}

impl Concurrency {
    /// Upper bound, e.g. to size the HTTP connection limit.
    pub fn max(&self) -> usize {
        match *self {
            Self::Fixed(n) => n.max(1),
            Self::Auto { min, max } => max.max(min).max(1),
        }
    }
}

/// A snapshot of how the machine is being used.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SystemLoad {
    /// Whole-system CPU use since the previous sample, 0.0..=1.0.
    pub cpu_busy: f32,
    /// Time since the last keyboard/mouse input.
    pub user_idle: Duration,
    pub on_battery: bool,
}

/// Source of `SystemLoad` samples (OS-specific; `None` when unknown).
pub trait LoadProbe: Send + Sync {
    fn sample(&self) -> Option<SystemLoad>;
}

/// Input counts as "in use" if it happened this recently.
const ACTIVE: Duration = Duration::from_secs(60);
/// No input for this long: the PC is idle, go full speed.
const IDLE: Duration = Duration::from_secs(5 * 60);
const CPU_HIGH: f32 = 0.75;
/// After a throttle, don't grow again for this long.
const COOLDOWN: Duration = Duration::from_secs(30);

pub struct Controller {
    mode: Concurrency,
    current: usize,
    slow_start: bool,
    last_throttles: u64,
    hold_until: Option<Instant>,
    reason: String,
}

impl Controller {
    pub fn new(mode: Concurrency) -> Self {
        let current = match mode {
            Concurrency::Fixed(n) => n.max(1),
            Concurrency::Auto { min, .. } => min.max(1),
        };
        Self {
            mode,
            current,
            slow_start: true,
            last_throttles: 0,
            hold_until: None,
            reason: match mode {
                Concurrency::Fixed(_) => "fixed".into(),
                Concurrency::Auto { .. } => "starting".into(),
            },
        }
    }

    pub fn workers(&self) -> usize {
        self.current
    }

    /// Why the current value was chosen (shown by `holo status --stream`).
    pub fn reason(&self) -> &str {
        &self.reason
    }

    pub fn is_auto(&self) -> bool {
        matches!(self.mode, Concurrency::Auto { .. })
    }

    /// Re-evaluate. `throttles` is the provider's running total of 429/503s.
    pub fn update(
        &mut self,
        now: Instant,
        backlog: u64,
        throttles: u64,
        load: Option<SystemLoad>,
    ) -> usize {
        let Concurrency::Auto { min, max } = self.mode else {
            return self.current;
        };
        let min = min.max(1);
        let max = max.max(min);

        let (mut ceiling, mut why) = match load {
            None => (max, "load unknown".to_string()),
            Some(l) if l.user_idle >= IDLE => (max, "PC idle".into()),
            Some(l) if l.user_idle < ACTIVE && l.cpu_busy >= CPU_HIGH => {
                (min, format!("PC busy (CPU {:.0}%)", l.cpu_busy * 100.0))
            }
            Some(l) if l.user_idle < ACTIVE => (max / 2, "PC in use".into()),
            Some(_) => (max * 3 / 4, "PC idle < 5 min".into()),
        };
        if load.is_some_and(|l| l.on_battery) {
            ceiling /= 2;
            why.push_str(", on battery");
        }
        let ceiling = ceiling.clamp(min, max);
        let want = usize::try_from(backlog)
            .unwrap_or(usize::MAX)
            .clamp(min, ceiling);

        if throttles > self.last_throttles {
            self.last_throttles = throttles;
            self.slow_start = false;
            self.hold_until = Some(now + COOLDOWN);
            self.current = (self.current / 2).clamp(min, ceiling);
            self.reason = "server asked to slow down (429/503)".into();
            return self.current;
        }
        let holding = self.hold_until.is_some_and(|t| now < t);

        if self.current > want {
            self.current = want;
        } else if self.current < want && !holding {
            self.current = if self.slow_start {
                (self.current * 2).min(want)
            } else {
                self.current + 1
            };
        }
        self.reason = if holding {
            format!("{why}, holding after throttling")
        } else {
            format!("{why}, queue {backlog}")
        };
        self.current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTO: Concurrency = Concurrency::Auto { min: 1, max: 16 };

    fn idle() -> Option<SystemLoad> {
        Some(SystemLoad {
            cpu_busy: 0.1,
            user_idle: Duration::from_secs(600),
            on_battery: false,
        })
    }

    fn in_use(cpu: f32) -> Option<SystemLoad> {
        Some(SystemLoad {
            cpu_busy: cpu,
            user_idle: Duration::from_secs(1),
            on_battery: false,
        })
    }

    #[test]
    fn fixed_never_changes() {
        let mut c = Controller::new(Concurrency::Fixed(4));
        assert_eq!(c.update(Instant::now(), 1000, 5, idle()), 4);
        assert_eq!(c.update(Instant::now(), 0, 9, in_use(1.0)), 4);
    }

    #[test]
    fn slow_start_doubles_up_to_max_when_idle() {
        let mut c = Controller::new(AUTO);
        let t = Instant::now();
        let steps: Vec<_> = (0..6).map(|_| c.update(t, 10_000, 0, idle())).collect();
        assert_eq!(steps, [2, 4, 8, 16, 16, 16]);
    }

    #[test]
    fn never_more_workers_than_queued_ops() {
        let mut c = Controller::new(AUTO);
        let t = Instant::now();
        for _ in 0..5 {
            c.update(t, 3, 0, idle());
        }
        assert_eq!(c.workers(), 3);
        assert_eq!(c.update(t, 0, 0, idle()), 1);
    }

    #[test]
    fn backs_off_when_the_user_is_working() {
        let mut c = Controller::new(AUTO);
        let t = Instant::now();
        for _ in 0..5 {
            c.update(t, 10_000, 0, idle());
        }
        assert_eq!(c.workers(), 16);
        assert_eq!(c.update(t, 10_000, 0, in_use(0.2)), 8);
        assert_eq!(c.update(t, 10_000, 0, in_use(0.9)), 1);
        assert!(c.reason().contains("busy"), "{}", c.reason());
    }

    #[test]
    fn battery_halves_the_ceiling() {
        let mut c = Controller::new(AUTO);
        let t = Instant::now();
        let on_battery = idle().map(|l| SystemLoad {
            on_battery: true,
            ..l
        });
        for _ in 0..6 {
            c.update(t, 10_000, 0, on_battery);
        }
        assert_eq!(c.workers(), 8);
    }

    #[test]
    fn throttling_halves_then_holds_then_grows_linearly() {
        let mut c = Controller::new(AUTO);
        let t = Instant::now();
        for _ in 0..5 {
            c.update(t, 10_000, 0, idle());
        }
        assert_eq!(c.update(t, 10_000, 1, idle()), 8);
        // Same total: no new throttle, but still holding.
        assert_eq!(c.update(t + Duration::from_secs(5), 10_000, 1, idle()), 8);
        let later = t + COOLDOWN + Duration::from_secs(1);
        assert_eq!(c.update(later, 10_000, 1, idle()), 9);
        assert_eq!(c.update(later, 10_000, 1, idle()), 10);
    }
}
