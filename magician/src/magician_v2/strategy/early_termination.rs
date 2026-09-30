//! Early termination detection for exploration strategies
//!
//! This module provides utilities for detecting when exploration should be
//! terminated early to save resources while maintaining solution quality. It
//! implements several detection mechanisms:
//!
//! - **Diminishing Returns**: Detects when the rate of confidence improvement
//!   falls below a threshold
//! - **Plateau Detection**: Identifies when confidence has stopped improving
//! - **Cost-Benefit Analysis**: Determines if continued exploration is worth
//!   the resource cost
//!
//! These mechanisms help guided search strategies avoid wasting
//! resources on exploration that is unlikely to yield better results.

use std::collections::VecDeque;

/// Detects diminishing returns in confidence improvements over a sliding window
///
/// Tracks confidence values over recent iterations and calculates the
/// improvement rate. When the improvement rate falls below a threshold,
/// diminishing returns are detected.
///
/// # Example
/// ```no_run
/// use magician::magician_v2::strategy::early_termination::DiminishingReturnsDetector;
///
/// let mut detector = DiminishingReturnsDetector::new(10);
///
/// // Record confidence values
/// for confidence in &[0.5, 0.6, 0.65, 0.68, 0.69, 0.70, 0.70, 0.70] {
///     detector.record(*confidence);
/// }
///
/// // Check if improvement rate is below 0.01 per iteration
/// if detector.has_diminishing_returns(0.01) {
///     println!("Exploration showing diminishing returns");
/// }
/// ```
#[derive(Debug, Clone)]
pub struct DiminishingReturnsDetector {
    /// Sliding window of recent confidence values
    confidence_history: VecDeque<f32>,
    /// Size of the sliding window
    window_size: usize,
}

impl DiminishingReturnsDetector {
    /// Create a new diminishing returns detector with specified window size
    ///
    /// # Arguments
    /// * `window_size` - Number of recent iterations to track (typically 10-20)
    pub fn new(window_size: usize) -> Self {
        Self {
            confidence_history: VecDeque::with_capacity(window_size),
            window_size,
        }
    }

    /// Record a new confidence value
    ///
    /// Adds the confidence to the sliding window. If the window is full,
    /// the oldest value is removed.
    ///
    /// # Arguments
    /// * `confidence` - Confidence value from current iteration (0.0 to 1.0)
    pub fn record(&mut self, confidence: f32) {
        self.confidence_history.push_back(confidence);
        if self.confidence_history.len() > self.window_size {
            self.confidence_history.pop_front();
        }
    }

    /// Check if improvement rate has fallen below threshold (diminishing
    /// returns)
    ///
    /// Calculates the improvement rate over the window and compares it to the
    /// threshold. Returns true if the improvement per iteration is less
    /// than the threshold.
    ///
    /// # Arguments
    /// * `threshold` - Minimum improvement rate per iteration (e.g., 0.01 = 1%
    ///   per iteration)
    ///
    /// # Returns
    /// * `true` if improvement rate < threshold (diminishing returns detected)
    /// * `false` if improvement rate >= threshold (still improving well) or
    ///   insufficient data
    pub fn has_diminishing_returns(&self, threshold: f32) -> bool {
        if self.confidence_history.len() < self.window_size {
            return false; // Need full window for accurate assessment
        }

        let rate = self.improvement_rate();
        rate < threshold
    }

    /// Calculate the average improvement rate per iteration over the window
    ///
    /// # Returns
    /// * Average change in confidence per iteration
    /// * Positive values indicate improvement, negative indicate decline
    /// * Returns 0.0 if insufficient data
    pub fn improvement_rate(&self) -> f32 {
        if self.confidence_history.len() < 2 {
            return 0.0;
        }

        let first = self.confidence_history.front().unwrap();
        let last = self.confidence_history.back().unwrap();

        // Calculate improvement per iteration
        (last - first) / (self.confidence_history.len() - 1) as f32
    }

    /// Get the current window size
    pub fn window_size(&self) -> usize {
        self.window_size
    }

    /// Get the number of recorded values
    pub fn recorded_count(&self) -> usize {
        self.confidence_history.len()
    }

    /// Check if the window is full
    pub fn is_window_full(&self) -> bool {
        self.confidence_history.len() >= self.window_size
    }

    /// Calculate variance over the window
    ///
    /// Low variance indicates a plateau (values aren't changing much).
    /// High variance indicates volatility (values are fluctuating).
    ///
    /// # Returns
    /// * Variance of confidence values in the window
    /// * Returns 0.0 if insufficient data
    pub fn variance(&self) -> f32 {
        if self.confidence_history.is_empty() {
            return 0.0;
        }

        let mean: f32 =
            self.confidence_history.iter().sum::<f32>() / self.confidence_history.len() as f32;

        let variance: f32 = self
            .confidence_history
            .iter()
            .map(|&x| (x - mean).powi(2))
            .sum::<f32>()
            / self.confidence_history.len() as f32;

        variance
    }

    /// Check if values have plateaued (low variance)
    ///
    /// # Arguments
    /// * `threshold` - Maximum variance to consider a plateau (e.g., 0.001)
    ///
    /// # Returns
    /// * `true` if variance < threshold (plateau detected)
    /// * `false` otherwise
    pub fn has_plateaued(&self, threshold: f32) -> bool {
        if !self.is_window_full() {
            return false;
        }

        self.variance() < threshold
    }

    /// Reset the detector (clear all history)
    pub fn reset(&mut self) {
        self.confidence_history.clear();
    }
}

/// Configuration for early termination decisions
#[derive(Debug, Clone)]
pub struct EarlyTerminationConfig {
    /// Confidence threshold for "excellent" solution (typically 0.9)
    pub excellent_confidence: f32,
    /// Confidence threshold for "good" solution (typically 0.75)
    pub good_confidence: f32,
    /// Confidence threshold for "acceptable" solution (typically 0.6)
    pub acceptable_confidence: f32,
    /// Iterations without improvement to trigger termination for good solution
    pub good_plateau_iterations: u32,
    /// Iterations without improvement to trigger termination for acceptable
    /// solution
    pub acceptable_plateau_iterations: u32,
    /// Minimum iterations before allowing early termination
    pub min_iterations: u32,
    /// Threshold for diminishing returns detection (improvement per iteration)
    pub diminishing_returns_threshold: f32,
    /// Minimum confidence to apply diminishing returns termination
    pub diminishing_returns_min_confidence: f32,
}

impl Default for EarlyTerminationConfig {
    fn default() -> Self {
        Self {
            excellent_confidence: 0.9,
            good_confidence: 0.75,
            acceptable_confidence: 0.6,
            good_plateau_iterations: 15,
            acceptable_plateau_iterations: 25,
            min_iterations: 5,
            diminishing_returns_threshold: 0.01,
            diminishing_returns_min_confidence: 0.65,
        }
    }
}

impl EarlyTerminationConfig {
    /// Create a conservative configuration (allows more exploration)
    pub fn conservative() -> Self {
        Self {
            excellent_confidence: 0.95,
            good_confidence: 0.8,
            acceptable_confidence: 0.65,
            good_plateau_iterations: 20,
            acceptable_plateau_iterations: 30,
            min_iterations: 10,
            diminishing_returns_threshold: 0.005,
            diminishing_returns_min_confidence: 0.7,
        }
    }

    /// Create an aggressive configuration (terminates earlier)
    pub fn aggressive() -> Self {
        Self {
            excellent_confidence: 0.85,
            good_confidence: 0.7,
            acceptable_confidence: 0.55,
            good_plateau_iterations: 10,
            acceptable_plateau_iterations: 20,
            min_iterations: 3,
            diminishing_returns_threshold: 0.015,
            diminishing_returns_min_confidence: 0.6,
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_diminishing_returns_basic() {
        let mut detector = DiminishingReturnsDetector::new(5);

        // Record improving values
        detector.record(0.5);
        detector.record(0.6);
        detector.record(0.7);
        detector.record(0.8);
        detector.record(0.9);

        // Improvement rate = (0.9 - 0.5) / 4 = 0.1 per iteration
        assert!(!detector.has_diminishing_returns(0.05)); // 0.1 > 0.05, not diminishing
        assert!(detector.has_diminishing_returns(0.15)); // 0.1 < 0.15,
                                                         // diminishing
    }

    #[test]
    fn test_plateau_detection() {
        let mut detector = DiminishingReturnsDetector::new(5);

        // Record plateau (values barely changing)
        detector.record(0.75);
        detector.record(0.75);
        detector.record(0.76);
        detector.record(0.75);
        detector.record(0.75);

        // Should detect plateau (low variance)
        assert!(detector.has_plateaued(0.001));
        assert_eq!(detector.improvement_rate(), 0.0); // No net improvement
    }

    #[test]
    fn test_insufficient_data() {
        let mut detector = DiminishingReturnsDetector::new(10);

        // Only record 3 values (less than window size)
        detector.record(0.5);
        detector.record(0.6);
        detector.record(0.7);

        // Should not detect anything with insufficient data
        assert!(!detector.has_diminishing_returns(0.01));
        assert!(!detector.has_plateaued(0.001));
        assert!(!detector.is_window_full());
    }

    #[test]
    fn test_window_sliding() {
        let mut detector = DiminishingReturnsDetector::new(3);

        // Fill window
        detector.record(0.5);
        detector.record(0.6);
        detector.record(0.7);
        assert_eq!(detector.recorded_count(), 3);

        // Add more (should evict oldest)
        detector.record(0.8);
        assert_eq!(detector.recorded_count(), 3); // Still 3 (window size)

        // Improvement rate should be based on [0.6, 0.7, 0.8]
        // (0.8 - 0.6) / 2 = 0.1
        let rate = detector.improvement_rate();
        assert!((rate - 0.1).abs() < 0.01);
    }

    #[test]
    fn test_reset() {
        let mut detector = DiminishingReturnsDetector::new(5);

        detector.record(0.5);
        detector.record(0.6);
        detector.record(0.7);
        assert_eq!(detector.recorded_count(), 3);

        detector.reset();
        assert_eq!(detector.recorded_count(), 0);
        assert!(!detector.is_window_full());
    }

    #[test]
    fn test_config_defaults() {
        let config = EarlyTerminationConfig::default();
        assert_eq!(config.excellent_confidence, 0.9);
        assert_eq!(config.good_confidence, 0.75);
        assert_eq!(config.min_iterations, 5);
    }

    #[test]
    fn test_config_conservative() {
        let config = EarlyTerminationConfig::conservative();
        assert!(
            config.excellent_confidence > EarlyTerminationConfig::default().excellent_confidence
        );
        assert!(config.min_iterations > EarlyTerminationConfig::default().min_iterations);
    }

    #[test]
    fn test_config_aggressive() {
        let config = EarlyTerminationConfig::aggressive();
        assert!(
            config.excellent_confidence < EarlyTerminationConfig::default().excellent_confidence
        );
        assert!(config.min_iterations < EarlyTerminationConfig::default().min_iterations);
    }
}
