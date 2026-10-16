//! Platt scaling and calibration error.
//!
//! Serving already applies `sigmoid(platt_a * linear + platt_b)`. This module
//! fits those two scalars and reports the ECE the trainer gates on.

use super::logistic::sigmoid;

pub fn fit_platt(scores: &[f64], labels: &[f64]) -> (f64, f64) {
    assert_eq!(scores.len(), labels.len(), "scores and labels must align");
    let mut a = 1.0;
    let mut b = 0.0;
    if scores.is_empty() {
        return (a, b);
    }
    let scale = 1.0 / scores.len() as f64;
    for step in 0..1000 {
        let rate = 0.1 / ((step + 1) as f64).sqrt();
        let mut da = 0.0;
        let mut db = 0.0;
        for (&score, &label) in scores.iter().zip(labels) {
            let error = sigmoid(a * score + b) - label;
            da += error * score;
            db += error;
        }
        a -= rate * da * scale;
        b -= rate * db * scale;
    }
    (a, b)
}

pub fn platt(score: f64, a: f64, b: f64) -> f64 {
    sigmoid(a * score + b)
}

pub fn expected_calibration_error(probs: &[f64], labels: &[f64], bins: usize) -> f64 {
    assert_eq!(probs.len(), labels.len(), "probs and labels must align");
    if probs.is_empty() || bins == 0 {
        return 0.0;
    }
    let total = probs.len() as f64;
    let mut error = 0.0;
    for index in 0..bins {
        let low = index as f64 / bins as f64;
        let high = (index + 1) as f64 / bins as f64;
        let mut count = 0.0;
        let mut confidence = 0.0;
        let mut accuracy = 0.0;
        for (&probability, &label) in probs.iter().zip(labels) {
            let in_bin = if index + 1 == bins {
                (low..=1.0).contains(&probability)
            } else {
                (low..high).contains(&probability)
            };
            if !in_bin {
                continue;
            }
            count += 1.0;
            confidence += probability;
            accuracy += label;
        }
        if count == 0.0 {
            continue;
        }
        error += (count / total) * ((confidence / count) - (accuracy / count)).abs();
    }
    error
}

pub fn roc_auc(scores: &[f64], labels: &[f64]) -> f64 {
    assert_eq!(scores.len(), labels.len(), "scores and labels must align");
    let mut positives = Vec::new();
    let mut negatives = Vec::new();
    for (&score, &label) in scores.iter().zip(labels) {
        if label >= 0.5 {
            positives.push(score);
        } else {
            negatives.push(score);
        }
    }
    if positives.is_empty() || negatives.is_empty() {
        return 0.5;
    }
    let mut concordant = 0.0;
    let mut ties = 0.0;
    for positive in &positives {
        for negative in &negatives {
            if positive > negative {
                concordant += 1.0;
            } else if (positive - negative).abs() <= f64::EPSILON {
                ties += 1.0;
            }
        }
    }
    (concordant + 0.5 * ties) / (positives.len() * negatives.len()) as f64
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn scores() -> Vec<f64> {
        let mut scores = vec![2.5; 80];
        scores.extend(std::iter::repeat_n(-2.5, 20));
        scores
    }

    fn labels() -> Vec<f64> {
        // 20% of the high-scoring band is actually positive.
        let mut labels = vec![0.0; 64];
        labels.extend(std::iter::repeat_n(1.0, 16));
        labels.extend(std::iter::repeat_n(0.0, 20));
        labels
    }

    fn probs() -> Vec<f64> {
        vec![0.1, 0.15, 0.2, 0.8, 0.85, 0.9]
    }

    #[test]
    fn calibration_maps_scores_toward_observed_frequency() {
        // 20% of high-scoring items were actually positive; calibrated output for
        // that score band must be near 0.2, not near the raw score.
        let (a, b) = fit_platt(&scores(), &labels());
        let calibrated = platt(2.5, a, b);
        assert!((calibrated - 0.2).abs() < 0.1, "got {calibrated}");
    }

    #[test]
    fn calibration_error_is_measurable() {
        // The trainer gates on this, so it has to be computable, not eyeballed.
        assert!(expected_calibration_error(&probs(), &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0], 10) < 1.0);
    }
}
