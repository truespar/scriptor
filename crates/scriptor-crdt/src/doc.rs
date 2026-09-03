//! The [`CollabDoc`] API surface, split by what it operates on.
//!
//! Each module here carries one `impl CollabDoc` block. The struct itself stays in the crate root,
//! which is deliberate: Rust makes a private field visible to the defining module and every
//! descendant, so these modules reach `self.doc` and the rest without any of it becoming
//! `pub(crate)`. Grouping is by document construct rather than by layer, because that is how the
//! OOXML concepts divide - a change to how tables are tracked touches `tables` and `review`, not
//! nine files.

mod comments;
mod edit;
mod fields;
mod headers_footers;
mod images;
mod lifecycle;
mod numbering;
mod import;
mod outline;
mod page;
mod review;
mod save;
mod styles;
mod suggest;
mod tables;
