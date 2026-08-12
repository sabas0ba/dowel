//! ビルド層。ターゲットからアクショングラフを作り、実行する。

pub mod action;
pub mod backend;
pub mod bench;
pub mod compdb;
pub mod debug;
pub mod dump;
pub mod exec;
pub mod exports;
pub mod glob;
pub mod install;
pub mod migrate;
pub mod plan;
pub mod probe;
pub mod testing;
pub mod toolstyle;

pub use action::{Action, ActionId, ActionKind};
pub use backend::{Backend, BuildGraph, Step};
pub use plan::{build_dir, flatten_strs, plan, CompileCommand, Plan};
pub use testing::{Launcher, Outcome};
