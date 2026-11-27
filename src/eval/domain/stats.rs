use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Counts {
    pub tp: u32,
    #[serde(rename = "fn")]
    pub missed: u32,
    pub fp: u32,
    pub unlabeled: u32,
}

impl Counts {
    pub fn precision(&self) -> Option<f64> {
        ratio(self.tp, self.tp + self.fp)
    }

    pub fn recall(&self) -> Option<f64> {
        ratio(self.tp, self.tp + self.missed)
    }

    pub fn f1(&self) -> Option<f64> {
        ratio(2 * self.tp, 2 * self.tp + self.fp + self.missed)
    }
}

impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, other: Self) {
        self.tp += other.tp;
        self.missed += other.missed;
        self.fp += other.fp;
        self.unlabeled += other.unlabeled;
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CriterionCounts {
    pub total: u32,
    pub correct: u32,
    pub incorrect: u32,
    pub unclear: u32,
}

impl CriterionCounts {
    pub fn accuracy(&self) -> Option<f64> {
        ratio(self.correct, self.total)
    }

    pub fn unverifiable_rate(&self) -> Option<f64> {
        ratio(self.unclear, self.total)
    }
}

impl std::ops::AddAssign for CriterionCounts {
    fn add_assign(&mut self, other: Self) {
        self.total += other.total;
        self.correct += other.correct;
        self.incorrect += other.incorrect;
        self.unclear += other.unclear;
    }
}

pub fn ratio(numerator: u32, denominator: u32) -> Option<f64> {
    (denominator > 0).then(|| f64::from(numerator) / f64::from(denominator))
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Stat {
    pub mean: Option<f64>,
    pub stddev: Option<f64>,
    pub runs: u32,
}

impl Stat {
    pub fn of(values: impl IntoIterator<Item = Option<f64>>) -> Self {
        let values: Vec<f64> = values.into_iter().flatten().collect();
        if values.is_empty() {
            return Self::default();
        }
        let count = values.len() as f64;
        let mean = values.iter().sum::<f64>() / count;
        let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / count;
        Self { mean: Some(mean), stddev: Some(variance.sqrt()), runs: values.len() as u32 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_derive_precision_recall_and_f1() {
        let counts = Counts { tp: 3, missed: 1, fp: 1, unlabeled: 7 };

        assert_eq!(counts.precision(), Some(0.75));
        assert_eq!(counts.recall(), Some(0.75));
        assert_eq!(counts.f1(), Some(0.75));
    }

    #[test]
    fn empty_denominators_are_undefined_not_zero() {
        let counts = Counts::default();

        assert_eq!((counts.precision(), counts.recall(), counts.f1()), (None, None, None));
        assert_eq!(Counts { fp: 2, ..Counts::default() }.precision(), Some(0.0));
        assert_eq!(Counts { missed: 1, ..Counts::default() }.f1(), Some(0.0));
        assert_eq!(CriterionCounts::default().accuracy(), None);
    }

    #[test]
    fn criterion_counts_derive_accuracy_and_unverifiable_rate() {
        let counts = CriterionCounts { total: 4, correct: 2, incorrect: 1, unclear: 1 };

        assert_eq!(counts.accuracy(), Some(0.5));
        assert_eq!(counts.unverifiable_rate(), Some(0.25));
    }

    #[test]
    fn counts_accumulate() {
        let mut total = Counts { tp: 1, missed: 1, fp: 1, unlabeled: 1 };
        total += Counts { tp: 2, missed: 0, fp: 3, unlabeled: 4 };
        let mut criteria = CriterionCounts { total: 1, correct: 1, incorrect: 0, unclear: 0 };
        criteria += CriterionCounts { total: 2, correct: 0, incorrect: 1, unclear: 1 };

        assert_eq!(total, Counts { tp: 3, missed: 1, fp: 4, unlabeled: 5 });
        assert_eq!(criteria, CriterionCounts { total: 3, correct: 1, incorrect: 1, unclear: 1 });
    }

    #[test]
    fn stat_is_mean_and_population_stddev_over_defined_values() {
        let stat = Stat::of([Some(1.0), None, Some(3.0)]);

        assert_eq!(stat.mean, Some(2.0));
        assert_eq!(stat.stddev, Some(1.0));
        assert_eq!(stat.runs, 2);
        assert_eq!(Stat::of([None, None]), Stat::default());
    }

    #[test]
    fn counts_serialize_missed_as_fn() {
        let json = serde_json::to_value(Counts { tp: 1, missed: 2, fp: 3, unlabeled: 4 }).unwrap();

        assert_eq!(json, serde_json::json!({"tp": 1, "fn": 2, "fp": 3, "unlabeled": 4}));
    }
}
