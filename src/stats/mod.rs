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
    pub rx_history: VecDeque<(Instant, u64)>,
    pub tx_history: VecDeque<(Instant, u64)>,
    pub history_window: Duration,
    /// One `(sampled_at, rx_bps, tx_bps)` reading per `refresh()` call, most
    /// recent last, capped at 60 samples so a per-device chart can plot the
    /// last minute of rates without unbounded growth.
    pub rate_history: VecDeque<(Instant, f64, f64)>,
}

/// `rate_history` retains at most this many samples (see `refresh`).
const RATE_HISTORY_CAP: usize = 60;

impl BandwidthStats {
    pub fn new() -> Self {
        Self {
            rx_bps: 0.0,
            tx_bps: 0.0,
            current_bps: 0.0,
            peak_bps: 0.0,
            total_rx_bytes: 0,
            total_tx_bytes: 0,
            rx_history: VecDeque::new(),
            tx_history: VecDeque::new(),
            history_window: Duration::from_secs(10), // 10-second window
            rate_history: VecDeque::new(),
        }
    }

    pub fn update_rx(&mut self, bytes: u64) {
        let now = Instant::now();
        self.total_rx_bytes += bytes;
        self.rx_history.push_back((now, bytes));
        self.cleanup_old_entries();
        self.recalculate_rates();
    }

    pub fn update_tx(&mut self, bytes: u64) {
        let now = Instant::now();
        self.total_tx_bytes += bytes;
        self.tx_history.push_back((now, bytes));
        self.cleanup_old_entries();
        self.recalculate_rates();
    }

    fn cleanup_old_entries(&mut self) {
        let cutoff = Instant::now() - self.history_window;

        while let Some(&(timestamp, _)) = self.rx_history.front() {
            if timestamp < cutoff {
                self.rx_history.pop_front();
            } else {
                break;
            }
        }

        while let Some(&(timestamp, _)) = self.tx_history.front() {
            if timestamp < cutoff {
                self.tx_history.pop_front();
            } else {
                break;
            }
        }
    }

    fn recalculate_rates(&mut self) {
        let window_secs = self.history_window.as_secs_f64();

        // Calculate RX rate
        let rx_bytes: u64 = self.rx_history.iter().map(|(_, bytes)| bytes).sum();
        self.rx_bps = (rx_bytes as f64) / window_secs;

        // Calculate TX rate
        let tx_bytes: u64 = self.tx_history.iter().map(|(_, bytes)| bytes).sum();
        self.tx_bps = (tx_bytes as f64) / window_secs;

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

        self.rate_history
            .push_back((Instant::now(), self.rx_bps, self.tx_bps));
        while self.rate_history.len() > RATE_HISTORY_CAP {
            self.rate_history.pop_front();
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

        // First entry should be cleaned up
        assert_eq!(stats.rx_history.len(), 1);
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
        assert_eq!(stats.rate_history.len(), 60, "capped at 60 samples");
    }
}
