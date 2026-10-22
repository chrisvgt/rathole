use std::future::Future;
use std::time::{Duration, Instant};
use tokio::time;

/// Exponential backoff with jitter. The defaults match the ones of the `backoff` crate
#[derive(Debug, Clone)]
pub struct ExponentialBackoff {
    pub current_interval: Duration,
    pub initial_interval: Duration,
    pub randomization_factor: f64,
    pub multiplier: f64,
    pub max_interval: Duration,
    pub max_elapsed_time: Option<Duration>,
    pub start_time: Instant,
}

impl Default for ExponentialBackoff {
    fn default() -> Self {
        let initial_interval = Duration::from_millis(500);
        ExponentialBackoff {
            current_interval: initial_interval,
            initial_interval,
            randomization_factor: 0.5,
            multiplier: 1.5,
            max_interval: Duration::from_secs(60),
            max_elapsed_time: Some(Duration::from_secs(15 * 60)),
            start_time: Instant::now(),
        }
    }
}

impl ExponentialBackoff {
    pub fn reset(&mut self) {
        self.current_interval = self.initial_interval;
        self.start_time = Instant::now();
    }

    /// The duration to wait before the next retry, or `None` if the retries are exhausted
    pub fn next_backoff(&mut self) -> Option<Duration> {
        if let Some(max) = self.max_elapsed_time {
            if self.start_time.elapsed() > max {
                return None;
            }
        }

        let current = self.current_interval.as_secs_f64();
        let delta = self.randomization_factor * current;
        let (min, max) = (current - delta, current + delta);
        let next = min + rand::random::<f64>() * (max - min);

        self.increment_current_interval();
        Some(Duration::from_secs_f64(next.max(0.0)))
    }

    fn increment_current_interval(&mut self) {
        let max = self.max_interval.as_secs_f64();
        let current = self.current_interval.as_secs_f64();
        self.current_interval = if current >= max / self.multiplier {
            self.max_interval
        } else {
            Duration::from_secs_f64(current * self.multiplier)
        };
    }
}

/// Retry `operation` until it succeeds or the backoff is exhausted.
/// `notify` is called with the error and the delay before every retry
pub async fn retry_notify<I, E, Fn, Fut, N>(
    mut backoff: ExponentialBackoff,
    mut operation: Fn,
    mut notify: N,
) -> Result<I, E>
where
    Fn: FnMut() -> Fut,
    Fut: Future<Output = Result<I, E>>,
    N: FnMut(&E, Duration),
{
    backoff.reset();
    loop {
        match operation().await {
            Ok(v) => return Ok(v),
            Err(e) => match backoff.next_backoff() {
                Some(d) => {
                    notify(&e, d);
                    time::sleep(d).await;
                }
                None => return Err(e),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_intervals_grow_and_cap() {
        let mut b = ExponentialBackoff {
            randomization_factor: 0.0,
            max_interval: Duration::from_secs(2),
            max_elapsed_time: None,
            ..Default::default()
        };
        assert_eq!(b.next_backoff(), Some(Duration::from_millis(500)));
        assert_eq!(b.next_backoff(), Some(Duration::from_millis(750)));
        for _ in 0..20 {
            assert!(b.next_backoff().unwrap() <= Duration::from_secs(2));
        }
        assert_eq!(b.next_backoff(), Some(Duration::from_secs(2)));
        b.reset();
        assert_eq!(b.next_backoff(), Some(Duration::from_millis(500)));
    }

    #[test]
    fn test_jitter_stays_in_range() {
        let mut b = ExponentialBackoff::default();
        let d = b.next_backoff().unwrap();
        assert!(d >= Duration::from_millis(250) && d <= Duration::from_millis(750));
    }

    #[test]
    fn test_exhausted() {
        let mut b = ExponentialBackoff {
            max_elapsed_time: Some(Duration::ZERO),
            ..Default::default()
        };
        std::thread::sleep(Duration::from_millis(2));
        assert_eq!(b.next_backoff(), None);
    }

    #[tokio::test]
    async fn test_retry_until_success() {
        let mut calls = 0;
        let mut notified = 0;
        let backoff = ExponentialBackoff {
            initial_interval: Duration::from_millis(1),
            max_interval: Duration::from_millis(2),
            ..Default::default()
        };
        let r = retry_notify(
            backoff,
            || {
                calls += 1;
                let ok = calls >= 3;
                async move {
                    if ok {
                        Ok(42)
                    } else {
                        Err("fail")
                    }
                }
            },
            |_, _| notified += 1,
        )
        .await;
        assert_eq!(r, Ok(42));
        assert_eq!(notified, 2);
    }

    #[tokio::test]
    async fn test_retry_gives_up() {
        let backoff = ExponentialBackoff {
            initial_interval: Duration::from_millis(1),
            max_elapsed_time: Some(Duration::from_millis(5)),
            ..Default::default()
        };
        let r: Result<(), &str> = retry_notify(backoff, || async { Err("fail") }, |_, _| {}).await;
        assert_eq!(r, Err("fail"));
    }
}
