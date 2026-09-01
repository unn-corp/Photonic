mod a;
mod b;
mod c;
mod d;

/// Maximum amount of one-shot generated geometry that an MCP procedural tool
/// may materialize in a single request.
pub const MAX_GENERATED_WORK: usize = 10_000;

pub use {a::*, b::*, c::*, d::*};
