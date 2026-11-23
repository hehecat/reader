//! HTTP API（`/reader3/*` 及静态资源/媒体路由）

pub mod assets;
pub mod auth;
pub mod books;
pub mod features;
pub mod files;
pub mod router;
pub mod search;
pub mod sources;
pub mod sse;

pub use router::router;
