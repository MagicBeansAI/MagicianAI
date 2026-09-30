//! L2-regularized logistic regression.
//!
//! This is the same fit serving already consumes: coefficients, intercept, and
//! `l2_lambda` on an `ActionabilityModelSnapshot`. The algorithm is the
//! deterministic decaying-rate gradient descent from
//! `scripts/eval_attention_actionability.py` so a Rust run and a fixture run
//! produce comparable artifacts.

#[derive(Debug, Clone, PartialEq)]
pub struct LogisticFit {
    pub coefficients: Vec<f64>,
    pub intercept: f64,
}

pub fn fit_logistic(x: &[Vec<f64>], y: &[f64], l2: f64, steps: usize) -> LogisticFit {
    assert_eq!(x.len(), y.len(), "design matrix and labels must align");
    let width = x.first().map(Vec::len).unwrap_or(0);
    assert!(
        x.iter().all(|row| row.len() == width),
        "every row must have the same feature width"
    );
    let mut weights = vec![0.0; width];
    let mut intercept = 0.0;
    if x.is_empty() || width == 0 {
        return LogisticFit {
            coefficients: weights,
            intercept,
        };
    }
    let scale = 1.0 / x.len() as f64;
    for step in 0..steps {
        let rate = 0.2 / ((step + 1) as f64).sqrt();
        let mut gradients = vec![0.0; width];
        let mut intercept_gradient = 0.0;
        for (row, &label) in x.iter().zip(y) {
            let error = sigmoid(dot(row, &weights, intercept)) - label;
            intercept_gradient += error;
            for (gradient, &value) in gradients.iter_mut().zip(row) {
                *gradient += error * value;
            }
        }
        intercept -= rate * intercept_gradient * scale;
        for (weight, gradient) in weights.iter_mut().zip(&gradients) {
            *weight -= rate * (*gradient * scale + l2 * *weight);
        }
    }
    LogisticFit {
        coefficients: weights,
        intercept,
    }
}

pub fn linear_score(row: &[f64], coefficients: &[f64], intercept: f64) -> f64 {
    dot(row, coefficients, intercept)
}

pub fn sigmoid(value: f64) -> f64 {
    if value >= 0.0 {
        1.0 / (1.0 + (-value).exp())
    } else {
        let exp = value.exp();
        exp / (1.0 + exp)
    }
}

fn dot(row: &[f64], weights: &[f64], intercept: f64) -> f64 {
    intercept
        + row
            .iter()
            .zip(weights)
            .map(|(value, weight)| value * weight)
            .sum::<f64>()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn fixture() -> (Vec<Vec<f64>>, Vec<f64>) {
        (
            vec![
                vec![1.0, 0.0],
                vec![0.9, 0.1],
                vec![0.0, 1.0],
                vec![0.1, 0.9],
            ],
            vec![1.0, 1.0, 0.0, 0.0],
        )
    }

    #[test]
    fn separable_data_is_learned_and_regularization_shrinks_weights() {
        let x = vec![
            vec![1.0, 0.0],
            vec![0.9, 0.1],
            vec![0.0, 1.0],
            vec![0.1, 0.9],
        ];
        let y = vec![1.0, 1.0, 0.0, 0.0];

        let loose = fit_logistic(&x, &y, 0.001, 200);
        assert!(loose.coefficients[0] > loose.coefficients[1]);

        // L2 must actually bind: a heavier penalty produces smaller weights.
        let tight = fit_logistic(&x, &y, 10.0, 200);
        assert!(tight.coefficients[0].abs() < loose.coefficients[0].abs());
    }

    #[test]
    fn fitting_is_deterministic() {
        // A snapshot is an immutable artifact; identical inputs must produce an
        // identical model or its digest means nothing.
        let (x, y) = fixture();
        assert_eq!(
            fit_logistic(&x, &y, 0.1, 200),
            fit_logistic(&x, &y, 0.1, 200)
        );
    }
}
