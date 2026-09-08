//! モデル層。マニフェストをパッケージとターゲットの網に組み上げる。

pub mod dump;
pub mod fetch;
pub mod graph;
pub mod interface;
pub mod lock;
pub mod package;
pub mod persist;
pub mod pkgconfig;
pub mod query;
pub mod runner;
pub mod session;
pub mod target;
pub mod why;

pub use dowel_query::Stats as QueryStats;
pub use graph::Graph;
pub use package::{DepKind, Dependency, Package};
pub use query::Key as QueryKey;
pub use runner::Runner;
pub use session::{case_name_problem, Session};
pub use target::{ArtifactDecl, CaseDecl, HarnessDecl, PackageId, PropMap, Target, TargetId};
