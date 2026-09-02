use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalOrder {
    pub id: String,
    pub symbol: String,
    pub qty: f64,
    pub status: String,
    pub updated_at: DateTime<Utc>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reconciliation {
    pub checked_at: DateTime<Utc>,
    pub matched: usize,
    pub mismatched: usize,
    pub unknown: Vec<String>,
    pub broker_only: Vec<String>,
}
#[derive(Debug, Error)]
pub enum ReconcileError {
    #[error("duplicate external order id")]
    Duplicate,
}
pub fn reconcile(
    local: &[ExternalOrder],
    external: &[ExternalOrder],
) -> Result<Reconciliation, ReconcileError> {
    let mut ids = std::collections::HashSet::new();
    for x in external {
        if !ids.insert(x.id.clone()) {
            return Err(ReconcileError::Duplicate);
        }
    }
    let mut matched = 0;
    let mut mismatched = 0;
    let mut unknown = Vec::new();
    for l in local {
        match external.iter().find(|e| e.id == l.id) {
            Some(e)
                if e.symbol == l.symbol && (e.qty - l.qty).abs() < 1e-9 && e.status == l.status =>
            {
                matched += 1
            }
            Some(_) => mismatched += 1,
            None => unknown.push(l.id.clone()),
        }
    }
    let broker_only = external
        .iter()
        .filter(|e| !local.iter().any(|l| l.id == e.id))
        .map(|e| e.id.clone())
        .collect();
    Ok(Reconciliation {
        checked_at: Utc::now(),
        matched,
        mismatched,
        unknown,
        broker_only,
    })
}
