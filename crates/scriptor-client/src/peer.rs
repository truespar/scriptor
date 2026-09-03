//! The [`AgentPeer`] surface, split by what an agent does with it.
//!
//! Each module carries one `impl AgentPeer` block. The struct stays in the crate root, so these
//! reach its private fields without any of them widening. The division mirrors the loop an agent
//! actually runs: perceive the document, propose tracked changes against what it saw, review what
//! came back. `dto` is the same surface in JSON for non-Rust callers, and `regions` scopes it to a
//! header or footer story.

mod dto;
mod perceive;
mod propose;
mod regions;
mod review;
