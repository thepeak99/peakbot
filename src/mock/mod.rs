//! Mock layer for integration testing

pub mod completion_model;
pub mod response;

pub use completion_model::{MockCompletionModel, RecordedRequest};
pub use response::{MockResponse, Usage};
