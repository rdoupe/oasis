//! Shared wire protocol and presentation encoding code.

mod cell_symbol;
pub mod endpoint;
pub(crate) mod render_ansi;
pub(crate) mod surface_delta;
pub(crate) mod surface_reuse;
pub(crate) mod surface_scroll;
mod wire;

pub use cell_symbol::CellSymbol;
pub use wire::*;
