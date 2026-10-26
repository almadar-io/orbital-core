//! Running a query over rows held in memory.

use super::*;

// ----- running a query ------------------------------------------------------------------------

/// What a scan visitor tells its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanControl {
    Continue,
    Stop,
}

/// One run of a [`RowQuery`] over rows offered one at a time. Keeps the pointers of the matches awaiting selection
/// (every query is ordered, by default `createdAt` then `id`) and counts them for the total.
pub struct QueryRun<'q> {
    pub(super) query: &'q RowQuery,
    pub(super) matched: Vec<Arc<Value>>,
    pub(super) total: usize,
}

impl<'q> QueryRun<'q> {
    pub fn new(query: &'q RowQuery) -> Self {
        Self {
            query,
            matched: Vec::new(),
            total: 0,
        }
    }

    /// Offer one row.
    pub fn visit(&mut self, row: &Arc<Value>) -> ScanControl {
        let hit = self.query.policy.as_ref().is_none_or(|p| p.matches(row))
            && self.query.filter.as_ref().is_none_or(|p| p.matches(row));
        if !hit {
            return ScanControl::Continue;
        }
        self.total += 1;
        self.matched.push(Arc::clone(row));
        ScanControl::Continue
    }

    /// The window's pointers in order, and the total. A `nearest` query is the nearest `k` of the matches,
    /// with no order, offset or limit, and its total is the count returned (EXEC-FETCH-2).
    pub(super) fn window(mut self) -> Result<(Vec<Arc<Value>>, usize), String> {
        if let Some(nearest) = &self.query.nearest {
            let rows = nearest_rows(self.matched, nearest)?;
            let total = rows.len();
            return Ok((rows, total));
        }
        let offset = self.query.offset.unwrap_or(0);
        let order = effective_row_order(self.query);
        let cmp = |a: &Arc<Value>, b: &Arc<Value>| compare_rows(a, b, &order);
        let end = self.query.limit.map(|l| offset.saturating_add(l));
        if let Some(end) = end.filter(|e| *e < self.matched.len()) {
            if end == 0 {
                self.matched.clear();
            } else {
                self.matched.select_nth_unstable_by(end - 1, cmp);
                self.matched.truncate(end);
            }
        }
        self.matched.sort_by(cmp);
        let start = offset.min(self.matched.len());
        let mut window = self.matched.split_off(start);
        if let Some(limit) = self.query.limit {
            window.truncate(limit);
        }
        let total = if self.query.total == Some(RowTotalMode::None) {
            window.len()
        } else {
            self.total
        };
        Ok((window, total))
    }

    /// The page: only the window's rows are cloned.
    pub fn finish(self) -> Result<RowPage, String> {
        let (window, total) = self.window()?;
        Ok(RowPage {
            rows: window.iter().map(|r| (**r).clone()).collect(),
            total,
        })
    }

    /// The ids of the page, no row cloned.
    pub fn finish_ids(self) -> Result<RowIdPage, String> {
        let (window, total) = self.window()?;
        Ok(RowIdPage {
            ids: window.iter().map(|r| row_id(r)).collect(),
            total,
        })
    }
}

/// The reference run of a query over `rows`; fails only for a `nearest` query the rows cannot answer.
pub fn run_query<'a>(rows: impl IntoIterator<Item = &'a Arc<Value>>, query: &RowQuery) -> Result<RowPage, String> {
    let mut run = QueryRun::new(query);
    for row in rows {
        if run.visit(row) == ScanControl::Stop {
            break;
        }
    }
    run.finish()
}

/// The reference run of an ids-only query over `rows`: the same window as [`run_query`].
pub fn run_ids<'a>(rows: impl IntoIterator<Item = &'a Arc<Value>>, query: &RowQuery) -> Result<RowIdPage, String> {
    let mut run = QueryRun::new(query);
    for row in rows {
        if run.visit(row) == ScanControl::Stop {
            break;
        }
    }
    run.finish_ids()
}

/// `a AND b`, either side optional.
pub fn and_predicates(a: Option<RowPredicate>, b: Option<RowPredicate>) -> Option<RowPredicate> {
    match (a, b) {
        (Some(a), Some(b)) => Some(RowPredicate::All { of: vec![a, b] }),
        (a, b) => a.or(b),
    }
}

/// A row set by reference resolved against the current rows: the ids of `reference.ids` that still match
/// the reference's query (and `policy`, the current viewer's `@read`), in the reference's order; `total` is a
/// true re-count of those matches, ignoring order and window.
pub fn resolve_row_set<'a>(
    rows: impl IntoIterator<Item = &'a Arc<Value>>,
    reference: &RowSetRef,
    policy: Option<&RowPredicate>,
) -> RowPage {
    let combined = and_predicates(reference.query.policy.clone(), policy.cloned());
    let query = RowQuery {
        policy: combined,
        filter: reference.query.filter.clone(),
        ..RowQuery::default()
    };
    let matching = run_query(rows, &query).expect("a query without nearest cannot fail");
    let by_id: std::collections::HashMap<String, &Value> =
        matching.rows.iter().map(|r| (row_id(r), r)).collect();
    let resolved: Vec<Value> = reference
        .ids
        .iter()
        .filter_map(|id| by_id.get(id).map(|r| (*r).clone()))
        .collect();
    let total = if reference.query.nearest.is_some() {
        resolved.len()
    } else {
        matching.total
    };
    RowPage {
        rows: resolved,
        total,
    }
}
