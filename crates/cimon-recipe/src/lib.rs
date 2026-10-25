//! Recipe format (docs/recipe-spec.md): parsing, layering (`extends`/`include`), static
//! checks, step selection and semantic diff. Pure — no network, no processes — so the
//! server's pre-save check, tool-call validation and a teammate's local
//! `cimon recipe check` reach the same conclusion from the same code.

pub mod catalog;
pub mod check;
pub mod diff;
pub mod model;
pub mod refs;
pub mod resolve;
pub mod select;
pub mod tree;
pub mod vars;

pub use catalog::{Catalog, KindDef};
pub use check::{check_tree, Diag, Report, Severity};
pub use model::*;
pub use resolve::{resolve_project, Resolved, Step};
pub use select::{select, Plan, PlannedStep, Rejection, SelectRequest};
pub use tree::{MemTree, Tree};
