//! 評価層。CST を型つき値に落とし、構成で具体化する。

pub mod codec;
pub mod config;
pub mod digest;
pub mod eval;
pub mod schema;
pub mod specialize;
pub mod strict;
pub mod value;

pub use config::{Config, Opt};
pub use digest::{props_digest, value_digest};
pub use eval::{eval, CfgRef, Document, Entry, Table};
pub use schema::{Block, Merge, TableKind};
pub use specialize::specialize;
pub use value::{CfgKey, Data, Ns, Origin, PathBase, PathValue, Prov, Site, Type, Value};
