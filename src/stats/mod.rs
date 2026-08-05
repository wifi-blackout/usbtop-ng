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
}

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

    pub fn get_utilization_percentage(&self, max_speed_bps: f64) -> f64 {
        if max_speed_bps > 0.0 {
            (self.current_bps / max_speed_bps * 100.0).min(100.0)
        } else {
            0.0
        }
    }

    pub fn reset(&mut self) {
        self.rx_bps = 0.0;
        self.tx_bps = 0.0;
        self.current_bps = 0.0;
        self.peak_bps = 0.0;
        self.total_rx_bytes = 0;
        self.total_tx_bytes = 0;
        self.rx_history.clear();
        self.tx_history.clear();
    }

    pub fn get_history_data(&self, max_points: usize) -> Vec<(f64, f64, f64)> {
        // Returns (timestamp_offset, rx_rate, tx_rate) tuples
        let mut combined_history = Vec::new();
        let now = Instant::now();

        // Combine RX and TX history by timestamp
        let mut rx_iter = self.rx_history.iter();
        let mut tx_iter = self.tx_history.iter();

        let mut current_rx = rx_iter.next();
        let mut current_tx = tx_iter.next();

        while current_rx.is_some() || current_tx.is_some() {
            match (current_rx, current_tx) {
                (Some((rx_time, rx_bytes)), Some((tx_time, tx_bytes))) => {
                    if rx_time <= tx_time {
                        let offset = now.duration_since(*rx_time).as_secs_f64();
                        combined_history.push((offset, *rx_bytes as f64, 0.0));
                        current_rx = rx_iter.next();
                    } else {
                        let offset = now.duration_since(*tx_time).as_secs_f64();
                        combined_history.push((offset, 0.0, *tx_bytes as f64));
                        current_tx = tx_iter.next();
                    }
                }
                (Some((rx_time, rx_bytes)), None) => {
                    let offset = now.duration_since(*rx_time).as_secs_f64();
                    combined_history.push((offset, *rx_bytes as f64, 0.0));
                    current_rx = rx_iter.next();
                }
                (None, Some((tx_time, tx_bytes))) => {
                    let offset = now.duration_since(*tx_time).as_secs_f64();
                    combined_history.push((offset, 0.0, *tx_bytes as f64));
                    current_tx = tx_iter.next();
                }
                (None, None) => break,
            }
        }

        // Limit to max_points
        if combined_history.len() > max_points {
            let skip = combined_history.len() - max_points;
            combined_history.drain(0..skip);
        }

        combined_history
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
    fn test_history_cleanup() {
        let mut stats = BandwidthStats::new();
        stats.history_window = Duration::from_millis(100);

        stats.update_rx(1000);
        sleep(Duration::from_millis(150));
        stats.update_rx(1000);

        // First entry should be cleaned up
        assert_eq!(stats.rx_history.len(), 1);
    }
}
