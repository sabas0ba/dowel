//! モデル層。マニフェストをパッケージとターゲットの網に組み上げる。

pub mod dump;
pub mod graph;
pub mod interface;
pub mod package;
pub mod persist;
pub mod query;
pub mod runner;
pub mod session;
pub mod target;
pub mod why;

pub use graph::Graph;
pub use package::{DepKind, Dependency, Package};
pub use query::Key as QueryKey;
pub use runner::Runner;
pub use session::Session;
pub use target::{PackageId, PropMap, Target, TargetId};
