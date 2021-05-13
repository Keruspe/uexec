use crate::future_holder::FutureHolder;

use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;

/* The common state containing all the futures */
#[derive(Clone, Default)]
pub(crate) struct State(Arc<Mutex<HashMap<u64, FutureHolder>>>);

impl State {
    pub(crate) fn register_future(&self, future: FutureHolder) {
        self.0.lock().insert(future.id(), future);
    }

    pub(crate) fn deregister_future(&self, id: u64) -> Option<FutureHolder> {
        self.0.lock().remove(&id)
    }

    pub(crate) fn cancel(&self, id: u64) {
        // If the future was pending -not pollable-, do nothing and just drop it
        drop(self.0.lock().remove(&id));
    }
}
