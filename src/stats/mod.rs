use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct BandwidthStats {
    pub rx_bps: f64,      // Bytes per second received (device to host)
    pub tx_bps: f64,      // Bytes per second transmitted (host to device)
    pub current_bps: f64, // Total current bandwidth
    pub peak_bps: f64,    // Peak bandwidth seen
    pub total_rx_bytes: u64,
    pub total_tx_bytes: u64,
    /// Received bytes summed into fixed [`BUCKET`]-long slots, oldest first:
    /// one `(bucket_start, bytes)` entry per slot rather than one per packet,
    /// so a busy device costs a bounded number of entries per window instead
    /// of one per URB.
    pub rx_buckets: VecDeque<(Instant, u64)>,
    /// Transmitted bytes, bucketed exactly like [`Self::rx_buckets`].
    pub tx_buckets: VecDeque<(Instant, u64)>,
    /// Running total of `rx_buckets`' byte counts. Maintained incrementally on
    /// add and eviction so the packet path never rescans the window; treat it
    /// as read-only unless you also fix up `rx_buckets`.
    pub rx_window_sum: u64,
    /// Running total of `tx_buckets`' byte counts; see [`Self::rx_window_sum`].
    pub tx_window_sum: u64,
    pub history_window: Duration,
    /// One `(sampled_at, rx_bps, tx_bps)` reading per `refresh()` call, most
    /// recent last, holding the last [`RATE_HISTORY_WINDOW`] of samples so a
    /// per-device chart can plot the last minute of rates at any refresh rate
    /// without unbounded growth.
    pub rate_history: VecDeque<(Instant, f64, f64)>,
}

/// `rate_history` retains samples from this far back (see `refresh`). Matches
/// the 60-second x-axis the per-device rate chart draws.
const RATE_HISTORY_WINDOW: Duration = Duration::from_secs(60);

/// Width of one accounting slot in `rx_buckets`/`tx_buckets`. Small enough
/// that the sliding window's edge is accurate to a quarter second, large
/// enough that even a saturated bus adds only a handful of entries per second.
const BUCKET: Duration = Duration::from_millis(250);

impl BandwidthStats {
    pub fn new() -> Self {
        Self {
            rx_bps: 0.0,
            tx_bps: 0.0,
            current_bps: 0.0,
            peak_bps: 0.0,
            total_rx_bytes: 0,
            total_tx_bytes: 0,
            rx_buckets: VecDeque::new(),
            tx_buckets: VecDeque::new(),
            rx_window_sum: 0,
            tx_window_sum: 0,
            history_window: Duration::from_secs(10), // 10-second window
            rate_history: VecDeque::new(),
        }
    }

    /// Account `bytes` of received traffic. O(1) amortized: the byte count
    /// lands in the current bucket and the window sum is adjusted in place —
    /// nothing rescans the window, however many packets arrive.
    pub fn update_rx(&mut self, bytes: u64) {
        let now = Instant::now();
        self.total_rx_bytes += bytes;
        evict_expired(
            &mut self.rx_buckets,
            &mut self.rx_window_sum,
            now,
            self.history_window,
        );
        accumulate(&mut self.rx_buckets, &mut self.rx_window_sum, now, bytes);
        self.recalculate_rates();
    }

    /// Account `bytes` of transmitted traffic; see [`Self::update_rx`].
    pub fn update_tx(&mut self, bytes: u64) {
        let now = Instant::now();
        self.total_tx_bytes += bytes;
        evict_expired(
            &mut self.tx_buckets,
            &mut self.tx_window_sum,
            now,
            self.history_window,
        );
        accumulate(&mut self.tx_buckets, &mut self.tx_window_sum, now, bytes);
        self.recalculate_rates();
    }

    fn cleanup_old_entries(&mut self) {
        let now = Instant::now();
        evict_expired(
            &mut self.rx_buckets,
            &mut self.rx_window_sum,
            now,
            self.history_window,
        );
        evict_expired(
            &mut self.tx_buckets,
            &mut self.tx_window_sum,
            now,
            self.history_window,
        );
    }

    /// Recompute the published rates from the two window sums. O(1): the sums
    /// are maintained by `accumulate`/`evict_expired`, never re-derived here.
    fn recalculate_rates(&mut self) {
        let window_secs = self.history_window.as_secs_f64();

        self.rx_bps = (self.rx_window_sum as f64) / window_secs;
        self.tx_bps = (self.tx_window_sum as f64) / window_secs;

        // Calculate total current bandwidth
        self.current_bps = self.rx_bps + self.tx_bps;

        // Update peak
        if self.current_bps > self.peak_bps {
            self.peak_bps = self.current_bps;
        }
    }

    /// Re-evaluate rates against the sliding window without new traffic,
    /// so idle devices decay to zero instead of freezing at their last rate.
    /// Also samples the freshly recalculated rates into `rate_history` for
    /// the per-device rate chart.
    pub fn refresh(&mut self) {
        self.cleanup_old_entries();
        self.recalculate_rates();

        let now = Instant::now();
        self.rate_history.push_back((now, self.rx_bps, self.tx_bps));
        // Age-based, not count-based: the chart's x-axis is 60 seconds, and
        // the tick rate is the user's `--refresh` choice, so a fixed sample
        // count would silently mean 15s at 250ms and 120s at 2000ms.
        if let Some(cutoff) = now.checked_sub(RATE_HISTORY_WINDOW) {
            while let Some(&(sampled_at, _, _)) = self.rate_history.front() {
                if sampled_at >= cutoff {
                    break;
                }
                self.rate_history.pop_front();
            }
        }
    }

    /// Current bandwidth as a percentage of `max_speed_bps`, clamped to 100%.
    /// `0.0` when `max_speed_bps` is non-positive (e.g. an unknown speed).
    pub fn get_utilization_percentage(&self, max_speed_bps: f64) -> f64 {
        if max_speed_bps > 0.0 {
            (self.current_bps / max_speed_bps * 100.0).min(100.0)
        } else {
            0.0
        }
    }
}

/// Drop every bucket that started before the window opened, taking its bytes
/// out of `sum` as it goes. Amortized O(1) per update: each bucket is created
/// once and evicted once, whatever the packet rate that filled it.
fn evict_expired(
    buckets: &mut VecDeque<(Instant, u64)>,
    sum: &mut u64,
    now: Instant,
    window: Duration,
) {
    // Early in a machine's uptime `now - window` can precede the monotonic
    // clock's origin; nothing can have expired yet in that case.
    let Some(cutoff) = now.checked_sub(window) else {
        return;
    };
    while let Some(&(start, bytes)) = buckets.front() {
        if start >= cutoff {
            break;
        }
        // Saturating because the fields are public: a caller that edits the
        // buckets by hand must not be able to wrap the sum below zero.
        *sum = sum.saturating_sub(bytes);
        buckets.pop_front();
    }
}

/// Add `bytes` to the newest bucket, opening a new one when that bucket's
/// [`BUCKET`] period has elapsed, and keep `sum` in step.
fn accumulate(buckets: &mut VecDeque<(Instant, u64)>, sum: &mut u64, now: Instant, bytes: u64) {
    match buckets.back_mut() {
        Some((start, total)) if now.duration_since(*start) < BUCKET => *total += bytes,
        _ => buckets.push_back((now, bytes)),
    }
    *sum += bytes;
}

/// Bytes accumulated over a sliding window, published as a rate. The same
/// bucket accounting as [`BandwidthStats`], for callers that need one
/// direction-less counter per key (per-endpoint stats) instead of a full
/// rx/tx/peak/history block.
#[derive(Debug, Clone)]
pub struct WindowCounter {
    buckets: VecDeque<(Instant, u64)>,
    sum: u64,
    window: Duration,
}

impl WindowCounter {
    pub fn new(window: Duration) -> Self {
        Self {
            buckets: VecDeque::new(),
            sum: 0,
            window,
        }
    }

    /// Account `bytes`. O(1) amortized, like [`BandwidthStats::update_rx`].
    pub fn add(&mut self, bytes: u64) {
        let now = Instant::now();
        evict_expired(&mut self.buckets, &mut self.sum, now, self.window);
        accumulate(&mut self.buckets, &mut self.sum, now, bytes);
    }

    /// Re-evaluate the window without new traffic, so idle counters decay.
    pub fn refresh(&mut self) {
        let now = Instant::now();
        evict_expired(&mut self.buckets, &mut self.sum, now, self.window);
    }

    /// Bytes per second over the window.
    ///
    /// `cfg(test)`-only for now: no production code reads a per-endpoint
    /// rate yet — it lands with a later task's endpoint display; verified
    /// here and ready for that wiring.
    #[cfg(test)]
    pub fn bps(&self) -> f64 {
        self.sum as f64 / self.window.as_secs_f64()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    #[test]
    fn test_bandwidth_calculation() {
        let mut stats = BandwidthStats::new();

        // Add some data
        stats.update_rx(1000);
        stats.update_tx(500);

        assert_eq!(stats.total_rx_bytes, 1000);
        assert_eq!(stats.total_tx_bytes, 500);
        assert!(stats.current_bps > 0.0);
        assert_eq!(stats.peak_bps, stats.current_bps);
    }

    #[test]
    fn refresh_decays_rates_to_zero_after_window() {
        let mut stats = BandwidthStats::new();
        stats.history_window = Duration::from_millis(50);
        stats.update_rx(1000);
        assert!(stats.rx_bps > 0.0);
        sleep(Duration::from_millis(80));
        stats.refresh();
        assert_eq!(stats.rx_bps, 0.0);
        assert_eq!(stats.current_bps, 0.0);
        assert_eq!(stats.total_rx_bytes, 1000); // totals are cumulative
    }

    #[test]
    fn test_history_cleanup() {
        let mut stats = BandwidthStats::new();
        stats.history_window = Duration::from_millis(100);

        stats.update_rx(1000);
        sleep(Duration::from_millis(150));
        stats.update_rx(1000);

        // The first bucket aged out of the window, so only the new one is left
        // and the rate reflects its bytes alone.
        assert_eq!(stats.rx_buckets.len(), 1);
        assert_eq!(stats.rx_window_sum, 1000);
        assert_eq!(stats.rx_bps, 1000.0 / stats.history_window.as_secs_f64());
    }

    /// The packet path must be driven by the running window sum, never by a
    /// rescan of the whole window: two updates inside the window add up, and an
    /// update after the window expires carries none of the old bytes.
    #[test]
    fn update_accumulates_into_a_windowed_sum() {
        let mut stats = BandwidthStats::new();
        stats.history_window = Duration::from_millis(120);

        stats.update_rx(1000);
        stats.update_rx(500);
        assert_eq!(stats.rx_window_sum, 1500);
        assert_eq!(
            stats.rx_buckets.len(),
            1,
            "updates within one bucket period coalesce into a single bucket"
        );
        assert_eq!(stats.rx_bps, 1500.0 / stats.history_window.as_secs_f64());

        sleep(Duration::from_millis(200));
        stats.update_rx(200);
        assert_eq!(stats.rx_window_sum, 200, "expired bytes leave the window");
        assert_eq!(stats.rx_buckets.len(), 1);
        assert_eq!(stats.rx_bps, 200.0 / stats.history_window.as_secs_f64());
    }

    /// Same accounting on the tx side, including that the two directions keep
    /// separate window sums.
    #[test]
    fn tx_updates_use_their_own_windowed_sum() {
        let mut stats = BandwidthStats::new();
        stats.update_tx(700);
        stats.update_rx(300);
        assert_eq!(stats.tx_window_sum, 700);
        assert_eq!(stats.rx_window_sum, 300);
        assert_eq!(stats.tx_bps, 700.0 / stats.history_window.as_secs_f64());
        assert_eq!(stats.current_bps, stats.rx_bps + stats.tx_bps);
    }

    #[test]
    fn utilization_percentage_50_percent_case() {
        let mut stats = BandwidthStats::new();
        stats.current_bps = 500.0;
        assert_eq!(stats.get_utilization_percentage(1000.0), 50.0);
    }

    #[test]
    fn utilization_percentage_clamps_at_100() {
        let mut stats = BandwidthStats::new();
        stats.current_bps = 5000.0;
        assert_eq!(stats.get_utilization_percentage(1000.0), 100.0);
    }

    #[test]
    fn utilization_percentage_zero_when_max_is_zero() {
        let mut stats = BandwidthStats::new();
        stats.current_bps = 500.0;
        assert_eq!(stats.get_utilization_percentage(0.0), 0.0);
    }

    #[test]
    fn refresh_samples_rate_history() {
        let mut stats = BandwidthStats::new();
        stats.update_rx(1000);
        stats.refresh();
        stats.refresh();
        assert_eq!(stats.rate_history.len(), 2);
        assert!(stats.rate_history[0].1 > 0.0); // rx_bps sampled
        for _ in 0..100 {
            stats.refresh();
        }
        assert_eq!(
            stats.rate_history.len(),
            102,
            "every sample from the last 60s is retained, however fast the tick"
        );
    }

    /// The rate chart claims a 60-second window, so eviction is by age, not by
    /// sample count — at a 250ms refresh a 60-sample cap would only hold 15s.
    #[test]
    fn refresh_evicts_rate_history_by_age() {
        let mut stats = BandwidthStats::new();
        let long_ago = Instant::now()
            .checked_sub(Duration::from_secs(70))
            .expect("monotonic clock has at least 70s of history");
        stats.rate_history.push_back((long_ago, 1.0, 2.0));
        stats.rate_history.push_back((Instant::now(), 3.0, 4.0));

        stats.refresh();

        assert_eq!(
            stats.rate_history.len(),
            2,
            "the 70s-old sample goes, the recent one and this tick's sample stay"
        );
        assert!(
            stats
                .rate_history
                .iter()
                .all(|(t, _, _)| t.elapsed() < Duration::from_secs(60)),
            "no sample older than the 60s window survives"
        );
    }

    #[test]
    fn window_counter_reports_rate_over_its_window() {
        let mut c = WindowCounter::new(Duration::from_secs(10));
        c.add(1_000);
        assert_eq!(c.bps(), 100.0);
    }

    #[test]
    fn window_counter_decays_to_zero_after_refresh() {
        let mut c = WindowCounter::new(Duration::from_millis(50));
        c.add(1_000);
        sleep(Duration::from_millis(80));
        c.refresh();
        assert_eq!(c.bps(), 0.0);
    }
}
