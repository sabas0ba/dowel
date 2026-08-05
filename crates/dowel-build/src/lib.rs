//! ビルド層。ターゲットからアクショングラフを作り、実行する。

pub mod action;
pub mod compdb;
pub mod dump;
pub mod exec;
pub mod glob;
pub mod migrate;
pub mod ninja;
pub mod plan;
pub mod testing;

pub use action::{Action, ActionId, ActionKind};
pub use exec::Executor;
pub use plan::{build_dir, flatten_strs, plan, CompileCommand, Plan};
pub use testing::{Launcher, Outcome};
