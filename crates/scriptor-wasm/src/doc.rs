//! The [`ScriptorDoc`] browser API, split by what each part of it does.
//!
//! Every module here carries one `#[wasm_bindgen] impl ScriptorDoc` block. Splitting an exported
//! type's impl across modules is ordinary Rust and ordinary wasm-bindgen: the macro emits one shim
//! per exported method and they merge into a single JS class. Private helpers sit in the same
//! blocks and the macro ignores them, which is where they already lived.
//!
//! Roughly in pipeline order: `relayout` produces the geometry, `paint` rasterizes it, `caret`
//! answers questions about it, and the rest are the editing surfaces the ribbon drives.

mod caret;
mod comments;
mod edit;
mod fields;
mod format;
mod hf;
mod images;
mod lists;
mod page_setup;
mod paint;
mod relayout;
mod review;
mod styles;
mod tables;
