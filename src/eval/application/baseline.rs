use std::str::FromStr;
use std::sync::Arc;

use super::ports::{EvalError, EvalStore};
use crate::eval::domain::compare::{Comparison, compare};
use crate::eval::domain::run::EvalRun;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BaselineRef {
    Latest,
    Id(i64),
}

impl FromStr for BaselineRef {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim() {
            "latest" => Ok(Self::Latest),
            other => other
                .parse::<i64>()
                .ok()
                .filter(|id| *id > 0)
                .map(Self::Id)
                .ok_or_else(|| format!("baseline must be 'latest' or an eval run id, got '{value}'")),
        }
    }
}

pub struct CompareToBaseline {
    store: Arc<dyn EvalStore>,
}

impl CompareToBaseline {
    pub fn new(store: Arc<dyn EvalStore>) -> Self {
        Self { store }
    }

    pub fn resolve(&self, reference: BaselineRef) -> Result<EvalRun, EvalError> {
        match reference {
            BaselineRef::Latest => self
                .store
                .latest()?
                .ok_or_else(|| EvalError::NotFound("no stored eval run to use as a baseline".into())),
            BaselineRef::Id(id) => {
                self.store.get(id)?.ok_or_else(|| EvalError::NotFound(format!("eval run {id} does not exist")))
            }
        }
    }

    pub fn execute(&self, current: &EvalRun, baseline: &EvalRun, max_drop: f64) -> Result<Comparison, EvalError> {
        if !(0.0..=1.0).contains(&max_drop) {
            return Err(EvalError::InvalidInput(format!("max drop must be between 0 and 1, got {max_drop}")));
        }
        Ok(compare(current, baseline, max_drop))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::application::test_support::{MemoryStore, sample_run};

    #[test]
    fn baseline_references_parse() {
        assert_eq!("latest".parse(), Ok(BaselineRef::Latest));
        assert_eq!(" 7 ".parse(), Ok(BaselineRef::Id(7)));
        for bad in ["", "0", "-1", "x"] {
            assert!(bad.parse::<BaselineRef>().is_err(), "{bad}");
        }
    }

    #[test]
    fn resolves_latest_and_by_id_or_reports_what_is_missing() {
        let store = Arc::new(MemoryStore::default());
        let use_case = CompareToBaseline::new(store.clone());
        assert!(matches!(use_case.resolve(BaselineRef::Latest), Err(EvalError::NotFound(_))));
        let first = store.save(&sample_run(&[true])).unwrap();
        let second = store.save(&sample_run(&[false])).unwrap();

        assert_eq!(use_case.resolve(BaselineRef::Latest).unwrap().id, second);
        assert_eq!(use_case.resolve(BaselineRef::Id(first)).unwrap().id, first);
        assert!(matches!(use_case.resolve(BaselineRef::Id(99)), Err(EvalError::NotFound(_))));
    }

    #[test]
    fn compares_and_validates_the_allowed_drop() {
        let use_case = CompareToBaseline::new(Arc::new(MemoryStore::default()));
        let (good, bad) = (sample_run(&[true, true]), sample_run(&[true, false]));

        assert!(use_case.execute(&bad, &good, 0.05).unwrap().regressed);
        assert!(!use_case.execute(&good, &good, 0.05).unwrap().regressed);
        for invalid in [-0.1, 1.5, f64::NAN] {
            assert!(matches!(use_case.execute(&good, &good, invalid), Err(EvalError::InvalidInput(_))));
        }
    }
}
