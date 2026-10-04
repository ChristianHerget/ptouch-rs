// SPDX-License-Identifier: GPL-3.0-or-later
//! Cooperative cancellation. It cannot recall bytes already sent to a printer.

use crate::{PtouchError, Result};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A one-way cancellation token. Create a fresh token for each operation.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    /// Request cancellation from any thread.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(PtouchError::Cancelled)
        } else {
            Ok(())
        }
    }
}
