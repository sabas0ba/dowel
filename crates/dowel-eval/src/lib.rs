//! 評価層。CST を型つき値に落とし、構成で具体化する。

pub mod codec;
pub mod config;
pub mod eval;
pub mod schema;
pub mod specialize;
pub mod strict;
pub mod value;

pub use config::{Config, Opt};
pub use eval::{eval, Document, Entry, Table};
pub use schema::{Block, Merge, TableKind};
pub use specialize::specialize;
pub use value::{Data, Origin, PathBase, PathValue, Prov, Site, Type, Value};
