use crate::{
    domain::{Execution, NewExecution},
    storage::{Result, Store},
};
use std::num::NonZeroU32;

/// Local reservation coordinator. No default limit or success gate is chosen here.
/// A future owner-started run must validate GitHub eligibility before calling reserve.
pub struct Coordinator {
    store: Store,
    concurrency_limit: NonZeroU32,
}

impl Coordinator {
    pub fn new(store: Store, concurrency_limit: NonZeroU32) -> Self {
        Self {
            store,
            concurrency_limit,
        }
    }

    pub async fn reserve(&self, execution: NewExecution) -> Result<Execution> {
        self.store.reserve(execution, self.concurrency_limit).await
    }
}
