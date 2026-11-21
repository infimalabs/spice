//! A board-wide topic tree grown from a bounded window of authored task evidence.

pub mod evidence;
mod graph;
pub mod kept;
mod lexical;
pub mod stop_words;
mod words;

/// Maximum number of prepared statements held in the filing window.
pub const CAPACITY: usize = 256;
