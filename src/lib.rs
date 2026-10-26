//! Predictive multi-chat on a phext coordinate map.
//!
//! One `.phext` file is the whole chat. A message is a scroll. A thread is
//! the collection/volume/book under a topic (library/shelf/series). Each
//! reader has interest coordinates; the feed ranks threads by distance to
//! those interests and by recency, then keeps any single author to a small
//! number of lines.

pub mod auth;
pub mod cache;
pub mod calm;
pub mod config;
pub mod coord;
pub mod error;
pub mod fabric;
pub mod invite;
pub mod limit;
pub mod link;
pub mod model;
pub mod oauth;
pub mod rank;
pub mod scrolls;
pub mod seed;
pub mod server;
pub mod store;

pub use error::Error;
