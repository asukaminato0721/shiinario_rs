//! Instruction layouts shared with the binary VM dispatch.

/// The operand layout of a supported binary SCN instruction.
/// Specialized layouts depend on the opcode, engine version, or encoded data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// A fixed number of encoded operands, including result destinations.
    Operands(usize),
    Dll,
    Movie,
    Sound,
    SurfaceCopy,
    DrawItem,
    DrawImages,
    VarArgs,
    Timer,
    TextPosition,
    RealExpression,
    Case,
    JumpTable,
    Declarations,
    LoopStart,
    LoopEnd,
    Parameters,
    CallArguments,
    Return,
    Conditional,
}

/// Return the declared layout without executing the instruction.
/// Unknown opcodes return `None`.
pub fn instruction_layout(opcode: u16) -> Option<Layout> {
    super::BinaryVm::instruction_layout(opcode)
}
