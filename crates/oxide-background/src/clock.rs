use std::time::{Duration, Instant};

const INPUT_RESUME_DURATION: Duration = Duration::from_millis(400);

#[derive(Default)]
pub(crate) struct PlaybackClock {
    position: Duration,
    resumed: Option<Instant>,
    easing_elapsed: Option<Duration>,
}

impl PlaybackClock {
    pub fn position(&self, now: Instant) -> Duration {
        self.position
            .saturating_add(self.resumed.map_or(Duration::ZERO, |start| {
                let elapsed = now.saturating_duration_since(start);
                self.easing_elapsed.map_or(elapsed, |age| {
                    eased_position(age.saturating_add(elapsed)).saturating_sub(eased_position(age))
                })
            }))
    }

    pub fn set_running(&mut self, running: bool, now: Instant) {
        match (running, self.resumed) {
            (true, None) => self.resumed = Some(now),
            (false, Some(start)) => {
                self.position = self.position(now);
                if let Some(age) = self.easing_elapsed {
                    let age = age.saturating_add(now.saturating_duration_since(start));
                    self.easing_elapsed = (age < INPUT_RESUME_DURATION).then_some(age);
                }
                self.resumed = None;
            }
            _ => {}
        }
    }

    pub fn pause_for_input(&mut self, now: Instant) {
        self.set_running(false, now);
        self.easing_elapsed = Some(Duration::ZERO);
    }
}

fn eased_position(elapsed: Duration) -> Duration {
    if elapsed >= INPUT_RESUME_DURATION {
        return elapsed - INPUT_RESUME_DURATION / 2;
    }
    let duration = INPUT_RESUME_DURATION.as_secs_f64();
    let t = elapsed.as_secs_f64() / duration;
    // Integrating smoothstep velocity keeps both position and speed continuous at the join.
    Duration::from_secs_f64(duration * (t * t * t - 0.5 * t * t * t * t))
}
