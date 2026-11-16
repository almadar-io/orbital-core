//! The step IR (P-039): the block form a code generator reads. A [`StepModule`] is one authored expression (a guard,
//! an effect, a tick body) lowered to functions of basic blocks; function 0 is the expression, the others are its
//! lifted lambdas. Every value is defined once and has a [`Repr`]. A join is a block with parameters.
//!
//! Failure: an instruction whose [`Op::raises`] is true may fail, and a failure leaves the function at once with the
//! error and the instruction's `source` (the language has no handler, so every failure edge goes to the one exit).
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::ir::effects::OirEffect;
use crate::ir::expr::{OirBinding, OirBindingRoot};
use crate::ir::layout::{PathStep, Repr};

pub mod lower;

pub use lower::{lower_step, StepRefusal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ValueId(pub u32);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct BlockId(pub u32);

/// A use of a value; `moved` when this is its last use, so the callee takes it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Arg {
    pub value: ValueId,
    pub moved: bool,
}

impl Arg {
    pub fn of(value: ValueId) -> Arg {
        Arg { value, moved: false }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Op {
    Null,
    Bool { value: bool },
    Number { value: f64 },
    Text { value: String },
    /// The value a binding root names, read along a resolved path.
    Read { root: OirBindingRoot, steps: Vec<PathStep> },
    /// A path read from a value this function holds.
    Path { base: ValueId, steps: Vec<PathStep> },
    /// A typed value as the dynamic `Value`.
    Box { value: ValueId },
    /// A dynamic value read as a boolean; any other kind fails (EXEC-EVAL-1).
    Truth { value: ValueId },
    /// Whether a value is null.
    IsNull { value: ValueId },
    /// A strict operator of the operator table on evaluated operands.
    Call { operator: String, args: Vec<Arg> },
    /// A `site` operator: evaluated operands with their literal marks, and a callable per lambda or lazy operand.
    SiteCall { operator: String, operands: Vec<SiteOperand> },
    /// An effect, with the values of its child expressions in the effect's own order.
    Effect { effect: Box<OirEffect>, operands: Vec<Arg> },
    /// An effect the operator table declares and the lowering leaves as a call: its head and its operand values.
    HostEffect { head: String, operands: Vec<Arg> },
    List { items: Vec<Arg> },
    Record { fields: Vec<(String, Arg)> },
    /// A lifted lambda (a function of this module) closed over the values it captures.
    Closure { function: u32, captures: Vec<Arg> },
}

/// One operand of a site call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "operand", rename_all = "kebab-case")]
pub enum SiteOperand {
    /// An evaluated operand; `literal` when it was written as a literal string.
    Value { arg: Arg, literal: bool },
    /// A function of this module the operator runs: a lambda, or an operand it evaluates on demand.
    Callable { function: u32, captures: Vec<Arg> },
}

impl Op {
    pub fn raises(&self) -> bool {
        matches!(self, Op::Truth { .. } | Op::Call { .. } | Op::SiteCall { .. } | Op::Effect { .. } | Op::HostEffect { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Inst {
    pub result: ValueId,
    pub repr: Repr,
    pub op: Op,
    /// Which node of the lowered expression this instruction computes: its index in the order the lowering first
    /// visits the evaluated nodes (a `let` binding list and its names are not evaluated and take no index).
    pub source: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "term", rename_all = "kebab-case")]
pub enum Terminator {
    Jump { to: BlockId, args: Vec<Arg> },
    Branch { condition: ValueId, then: BlockId, otherwise: BlockId },
    Return { value: Arg },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub params: Vec<(ValueId, Repr)>,
    pub insts: Vec<Inst>,
    pub terminator: Terminator,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepFunction {
    /// The lambda's parameters, then the values it captures; the expression itself takes none.
    pub params: Vec<(String, ValueId)>,
    /// How many of `params` are captures (the trailing ones).
    pub captures: u32,
    pub blocks: Vec<Block>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepModule {
    pub functions: Vec<StepFunction>,
}

struct Shown<'a>(&'a Arg);

impl fmt::Display for Shown<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.moved {
            write!(f, "move %{}", self.0.value.0)
        } else {
            write!(f, "%{}", self.0.value.0)
        }
    }
}

fn args(f: &mut fmt::Formatter<'_>, list: &[Arg]) -> fmt::Result {
    for (i, arg) in list.iter().enumerate() {
        if i > 0 {
            f.write_str(", ")?;
        }
        write!(f, "{}", Shown(arg))?;
    }
    Ok(())
}

fn steps(f: &mut fmt::Formatter<'_>, path: &[PathStep]) -> fmt::Result {
    for step in path {
        match step {
            PathStep::Slot { slot } => write!(f, ".#{slot}")?,
            PathStep::Index { index } => write!(f, "[{index}]")?,
            PathStep::Length => f.write_str(".length")?,
            PathStep::Key { key } => write!(f, ".{key}")?,
        }
    }
    Ok(())
}

fn repr(repr: &Repr) -> &'static str {
    match repr {
        Repr::Bool => "bool",
        Repr::Int { .. } => "int",
        Repr::Number => "number",
        Repr::Text => "text",
        Repr::Bytes => "bytes",
        Repr::List { .. } => "list",
        Repr::Tuple { .. } => "tuple",
        Repr::Record { .. } => "record",
        Repr::Map { .. } => "map",
        Repr::Optional { .. } => "optional",
        Repr::Union { .. } => "union",
        Repr::Row { .. } => "row",
        Repr::Named { .. } => "named",
        Repr::Boxed => "boxed",
    }
}

impl fmt::Display for Inst {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "  %{}: {} = ", self.result.0, repr(&self.repr))?;
        match &self.op {
            Op::Null => f.write_str("null")?,
            Op::Bool { value } => write!(f, "{value}")?,
            Op::Number { value } => write!(f, "{value}")?,
            Op::Text { value } => write!(f, "{value:?}")?,
            Op::Read { root, steps: path } => {
                write!(f, "read {}", OirBinding::new(root.clone(), Vec::new()))?;
                steps(f, path)?;
            }
            Op::Path { base, steps: path } => {
                write!(f, "path %{}", base.0)?;
                steps(f, path)?;
            }
            Op::Box { value } => write!(f, "box %{}", value.0)?,
            Op::Truth { value } => write!(f, "truth %{}", value.0)?,
            Op::IsNull { value } => write!(f, "is-null %{}", value.0)?,
            Op::Call { operator, args: list } => {
                write!(f, "call {operator}(")?;
                args(f, list)?;
                f.write_str(")")?;
            }
            Op::SiteCall { operator, operands } => {
                write!(f, "call-site {operator}(")?;
                for (i, operand) in operands.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    match operand {
                        SiteOperand::Value { arg, literal: true } => write!(f, "literal {}", Shown(arg))?,
                        SiteOperand::Value { arg, literal: false } => write!(f, "{}", Shown(arg))?,
                        SiteOperand::Callable { function, captures } => {
                            write!(f, "fn{function}[")?;
                            args(f, captures)?;
                            f.write_str("]")?;
                        }
                    }
                }
                f.write_str(")")?;
            }
            Op::Effect { effect, operands } => {
                write!(f, "effect {}(", effect.kind())?;
                args(f, operands)?;
                f.write_str(")")?;
            }
            Op::HostEffect { head, operands } => {
                write!(f, "host-effect {head}(")?;
                args(f, operands)?;
                f.write_str(")")?;
            }
            Op::List { items } => {
                f.write_str("list [")?;
                args(f, items)?;
                f.write_str("]")?;
            }
            Op::Record { fields } => {
                f.write_str("record {")?;
                for (i, (name, arg)) in fields.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{name}: {}", Shown(arg))?;
                }
                f.write_str("}")?;
            }
            Op::Closure { function, captures } => {
                write!(f, "closure fn{function}[")?;
                args(f, captures)?;
                f.write_str("]")?;
            }
        }
        if self.op.raises() {
            f.write_str(" !")?;
        }
        write!(f, "  ; {}", self.source)
    }
}

impl fmt::Display for StepModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, function) in self.functions.iter().enumerate() {
            write!(f, "fn{index}(")?;
            let own = function.params.len() - function.captures as usize;
            for (i, (name, value)) in function.params.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                if i >= own {
                    f.write_str("capture ")?;
                }
                write!(f, "{name} %{}", value.0)?;
            }
            f.write_str(") {\n")?;
            for (id, block) in function.blocks.iter().enumerate() {
                write!(f, "b{id}")?;
                if !block.params.is_empty() {
                    f.write_str("(")?;
                    for (i, (value, kind)) in block.params.iter().enumerate() {
                        if i > 0 {
                            f.write_str(", ")?;
                        }
                        write!(f, "%{}: {}", value.0, repr(kind))?;
                    }
                    f.write_str(")")?;
                }
                f.write_str(":\n")?;
                for inst in &block.insts {
                    writeln!(f, "{inst}")?;
                }
                match &block.terminator {
                    Terminator::Jump { to, args: list } => {
                        write!(f, "  jump b{}(", to.0)?;
                        args(f, list)?;
                        f.write_str(")\n")?;
                    }
                    Terminator::Branch { condition, then, otherwise } => {
                        writeln!(f, "  branch %{}, b{}, b{}", condition.0, then.0, otherwise.0)?;
                    }
                    Terminator::Return { value } => writeln!(f, "  return {}", Shown(value))?,
                }
            }
            f.write_str("}\n")?;
        }
        Ok(())
    }
}
