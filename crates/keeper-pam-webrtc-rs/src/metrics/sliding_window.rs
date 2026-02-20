//! Sliding window calculations for time-based metrics

use std::collections::VecDeque;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

/// Time window specification for sliding calculations
#[derive(Debug, Clone, Copy)]
#[allow(dead_code)]
pub enum TimeWindow {
    OneSecond,
    TenSeconds,
    OneMinute,
}

impl TimeWindow {
    #[allow(dead_code)]
    pub fn duration(&self) -> Duration {
        match self {
            TimeWindow::OneSecond => Duration::from_secs(1),
            TimeWindow::TenSeconds => Duration::from_secs(10),
            TimeWindow::OneMinute => Duration::from_secs(60),
        }
    }
}

/// Data point in a sliding window
#[derive(Debug, Clone)]
pub struct DataPoint {
    pub value: f64,
    pub timestamp: Instant,
}

impl DataPoint {
    pub fn new(value: f64) -> Self {
        Self {
            value,
            timestamp: Instant::now(),
        }
    }
}

/// Thread-safe sliding window for time-based calculations
#[derive(Debug)]
pub struct SlidingWindow {
    window_duration: Duration,
    data_points: Arc<RwLock<VecDeque<DataPoint>>>,
    max_size: usize,
}

impl SlidingWindow {
    /// Create a new sliding window
    #[allow(dead_code)]
    pub fn new(window: TimeWindow, max_size: usize) -> Self {
        Self {
            window_duration: window.duration(),
            data_points: Arc::new(RwLock::new(VecDeque::new())),
            max_size,
        }
    }

    /// Add a new data point to the window
    pub fn add(&self, value: f64) {
        let point = DataPoint::new(value);
        let now = Instant::now();

        if let Ok(mut points) = self.data_points.write() {
            // Remove expired data points
            while let Some(front) = points.front() {
                if now.duration_since(front.timestamp) > self.window_duration {
                    points.pop_front();
                } else {
                    break;
                }
            }

            // Add new point
            points.push_back(point);

            // Maintain max size (prevent unbounded growth)
            while points.len() > self.max_size {
                points.pop_front();
            }
        }
    }

    /// Calculate average value over the window
    pub fn average(&self) -> f64 {
        if let Ok(points) = self.data_points.read() {
            self.clean_expired_points(&points);
            if points.is_empty() {
                return 0.0;
            }
            let sum: f64 = points.iter().map(|p| p.value).sum();
            sum / points.len() as f64
        } else {
            0.0
        }
    }

    /// Calculate percentile over the window (0.0 - 1.0)
    pub fn percentile(&self, p: f64) -> f64 {
        if let Ok(points) = self.data_points.read() {
            self.clean_expired_points(&points);
            if points.is_empty() {
                return 0.0;
            }

            let mut values: Vec<f64> = points.iter().map(|point| point.value).collect();
            values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

            let index = ((values.len() as f64 - 1.0) * p).floor() as usize;
            values.get(index).copied().unwrap_or(0.0)
        } else {
            0.0
        }
    }

    /// Get 95th percentile
    pub fn p95(&self) -> f64 {
        self.percentile(0.95)
    }

    /// Get 99th percentile
    pub fn p99(&self) -> f64 {
        self.percentile(0.99)
    }

    /// Calculate rate of change (per second)
    pub fn rate(&self) -> f64 {
        if let Ok(points) = self.data_points.read() {
            self.clean_expired_points(&points);
            if points.len() < 2 {
                return 0.0;
            }

            let first = points.front().unwrap();
            let last = points.back().unwrap();
            let time_diff = last.timestamp.duration_since(first.timestamp).as_secs_f64();

            if time_diff > 0.0 {
                (last.value - first.value) / time_diff
            } else {
                0.0
            }
        } else {
            0.0
        }
    }

    /// Get maximum value in the window
    pub fn max(&self) -> f64 {
        if let Ok(points) = self.data_points.read() {
            self.clean_expired_points(&points);
            if points.is_empty() {
                return 0.0;
            }

            points
                .iter()
                .map(|p| p.value)
                .fold(f64::NEG_INFINITY, f64::max)
        } else {
            0.0
        }
    }

    /// Clean expired points (called internally)
    fn clean_expired_points(&self, _points: &VecDeque<DataPoint>) {
        // This would normally clean expired points, but since we have a read lock,
        // we can't modify. The add() method handles cleanup during writes.
        // This is kept for potential future optimizations with different locking strategies.
    }
}

/// Collection of sliding windows for different time periods
#[derive(Debug)]
pub struct MultiWindow {
    pub one_second: SlidingWindow,
    pub ten_seconds: SlidingWindow,
    pub one_minute: SlidingWindow,
}

impl MultiWindow {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self {
            one_second: SlidingWindow::new(TimeWindow::OneSecond, 100), // ~1.6 minutes at 1Hz
            ten_seconds: SlidingWindow::new(TimeWindow::TenSeconds, 360), // ~1 hour at 10s intervals
            one_minute: SlidingWindow::new(TimeWindow::OneMinute, 1440), // ~24 hours at 1min intervals
        }
    }

    /// Add a value to all windows
    pub fn add(&self, value: f64) {
        self.one_second.add(value);
        self.ten_seconds.add(value);
        self.one_minute.add(value);
    }
}
