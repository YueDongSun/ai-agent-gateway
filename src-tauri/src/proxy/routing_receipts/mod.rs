//! Private request-bound routing observations. Receipts never grant execution.

pub mod contract;
pub(crate) mod http;
mod observation;
mod store;

pub(crate) use observation::{
    check_admission, current, observe_generation, observe_selector, scope, RequestObservation,
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReceiptVersion {
    V1,
    V2,
}

impl ReceiptVersion {
    fn header(self) -> &'static str {
        match self {
            Self::V1 => "1",
            Self::V2 => "2",
        }
    }
}
pub use observation::{ProviderObservation, WireAttempt, WireResponse};
pub(crate) use store::{ReceiptRegistry, ReceiptSecret, RECEIPT_CAPACITY, RETENTION_SECONDS};
