//! The resolver tier: model plus display mode in, layout blocks out.
//!
//! Everything under here is a pure function of the CRDT model, the style tables and the current
//! review mode. None of it touches [`ScriptorDoc`] or any of its caches, which is what makes a
//! relayout reproducible and testable without a live document.

mod blocks;
mod colour;
mod float;
mod flow;
mod frame;

pub(crate) use blocks::*;
pub(crate) use colour::*;
pub(crate) use float::*;
pub(crate) use flow::*;
pub(crate) use frame::*;
