//! Lowering an `OirExpr` to the step IR. Control forms (`if`, `and`, `or`, `when`, `default`, `let`, `do`) become
//! blocks; a strict operator becomes a call on evaluated operands; a `site` operator becomes a site call, with a
//! lifted function for each lambda or lazily evaluated operand; an effect becomes one instruction over the values of
//! its child expressions. What has no value entry yet is refused by name, never approximated.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::collections::{BTreeMap, BTreeSet};

use super::{Arg, Block, BlockId, Inst, Op, SiteOperand, StepFunction, StepModule, Terminator, ValueId};
use crate::ir::expr::{OirBinaryOp, OirBinding, OirBindingRoot, OirExpr, OirUnaryOp};
use crate::ir::layout::{resolve_path, Repr};
use crate::language::registry::OperatorEvaluation;

/// Why an expression has no step form yet.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum StepRefusal {
    /// A form that only has meaning inside another (`unquote` outside a template, `fn` as a call head).
    Quoted(String),
    /// An operator that evaluates its own operands and has no entry over values.
    FormOperator(String),
    /// A call head the operator table gives no evaluation mode (not an evaluator operator).
    NoMode(String),
    /// A collection node (`Map`, `Filter`, `Reduce`) the canonical lowering does not produce.
    CollectionNode(&'static str),
    /// A form whose operands do not have the shape the form requires.
    Malformed(String),
}

/// How the operator table says a call head takes its operands.
#[derive(Debug, Clone, PartialEq)]
pub struct CallMode {
    pub evaluation: OperatorEvaluation,
    /// The operands a `site` operator evaluates on demand, by position.
    pub lazy: Vec<u32>,
}

/// What the operator table says a call head is.
#[derive(Debug, Clone, PartialEq)]
pub enum CallHead {
    /// An evaluator operator, with how it takes its operands.
    Operator(CallMode),
    /// An effect the kernel runs by name on its operand values.
    Effect,
}

/// The representations of the roots an expression may read; a root not given is a dynamic value.
#[derive(Debug, Clone, Default)]
pub struct RootLayouts {
    pub entity: Option<Repr>,
    pub payload: Option<Repr>,
    pub config: Option<Repr>,
}

struct Pending {
    params: Vec<(ValueId, Repr)>,
    insts: Vec<Inst>,
    terminator: Option<Terminator>,
}

struct Builder {
    params: Vec<(String, ValueId)>,
    own_params: usize,
    captured: Vec<ValueId>,
    blocks: Vec<Pending>,
    current: usize,
    next_value: u32,
    scopes: Vec<Vec<(String, ValueId)>>,
}

impl Builder {
    fn new(params: &[String]) -> Builder {
        let mut builder = Builder {
            params: Vec::new(),
            own_params: params.len(),
            captured: Vec::new(),
            blocks: vec![Pending { params: Vec::new(), insts: Vec::new(), terminator: None }],
            current: 0,
            next_value: 0,
            scopes: vec![Vec::new()],
        };
        for name in params {
            let value = builder.value();
            builder.params.push((name.clone(), value));
            builder.scopes[0].push((name.clone(), value));
        }
        builder
    }

    fn value(&mut self) -> ValueId {
        let id = ValueId(self.next_value);
        self.next_value += 1;
        id
    }

    fn block(&mut self, params: Vec<(ValueId, Repr)>) -> BlockId {
        self.blocks.push(Pending { params, insts: Vec::new(), terminator: None });
        BlockId((self.blocks.len() - 1) as u32)
    }

    fn emit(&mut self, repr: Repr, op: Op, source: u32) -> ValueId {
        let result = self.value();
        self.blocks[self.current].insts.push(Inst { result, repr, op, source });
        result
    }

    fn end(&mut self, terminator: Terminator) {
        self.blocks[self.current].terminator = Some(terminator);
    }

    fn local(&self, name: &str) -> Option<ValueId> {
        self.scopes.iter().rev().find_map(|scope| scope.iter().rev().find(|(bound, _)| bound == name).map(|(_, v)| *v))
    }

    fn finish(self) -> StepFunction {
        let mut function = StepFunction {
            captures: (self.params.len() - self.own_params) as u32,
            params: self.params,
            blocks: self
                .blocks
                .into_iter()
                .map(|pending| Block {
                    params: pending.params,
                    insts: pending.insts,
                    terminator: pending.terminator.expect("every block is ended"),
                })
                .collect(),
        };
        mark_moves(&mut function);
        function
    }
}

/// The uses in `op` that hand a value over, last first.
fn handed_over(op: &mut Op) -> Vec<&mut Arg> {
    match op {
        Op::Call { args, .. } | Op::Effect { operands: args, .. } | Op::HostEffect { operands: args, .. } | Op::List { items: args } | Op::Closure { captures: args, .. } => {
            args.iter_mut().rev().collect()
        }
        Op::Record { fields } => fields.iter_mut().rev().map(|(_, arg)| arg).collect(),
        Op::SiteCall { operands, .. } => operands
            .iter_mut()
            .rev()
            .flat_map(|operand| match operand {
                SiteOperand::Value { arg, .. } => vec![arg],
                SiteOperand::Callable { captures, .. } => captures.iter_mut().rev().collect(),
            })
            .collect(),
        Op::Path { .. } | Op::Box { .. } | Op::Truth { .. } | Op::IsNull { .. } => Vec::new(),
        Op::Null | Op::Bool { .. } | Op::Number { .. } | Op::Text { .. } | Op::Read { .. } => Vec::new(),
    }
}

fn uses(op: &Op) -> Vec<ValueId> {
    match op {
        Op::Call { args, .. } | Op::Effect { operands: args, .. } | Op::HostEffect { operands: args, .. } | Op::List { items: args } | Op::Closure { captures: args, .. } => {
            args.iter().map(|arg| arg.value).collect()
        }
        Op::Record { fields } => fields.iter().map(|(_, arg)| arg.value).collect(),
        Op::SiteCall { operands, .. } => operands
            .iter()
            .flat_map(|operand| match operand {
                SiteOperand::Value { arg, .. } => vec![arg.value],
                SiteOperand::Callable { captures, .. } => captures.iter().map(|arg| arg.value).collect(),
            })
            .collect(),
        Op::Path { base: value, .. } | Op::Box { value } | Op::Truth { value } | Op::IsNull { value } => vec![*value],
        Op::Null | Op::Bool { .. } | Op::Number { .. } | Op::Text { .. } | Op::Read { .. } => Vec::new(),
    }
}

/// A value whose every use is in the block that defines it is moved at its last use, when that use hands it over.
fn mark_moves(function: &mut StepFunction) {
    let params: Vec<ValueId> = function.params.iter().map(|(_, value)| *value).collect();
    let mut home = BTreeMap::new();
    for (index, block) in function.blocks.iter().enumerate() {
        for (value, _) in &block.params {
            home.insert(*value, index);
        }
        for inst in &block.insts {
            home.insert(inst.result, index);
        }
    }
    let mut leaves_home = BTreeSet::new();
    for (index, block) in function.blocks.iter().enumerate() {
        let mut used: Vec<ValueId> = block.insts.iter().flat_map(|inst| uses(&inst.op)).collect();
        match &block.terminator {
            Terminator::Jump { args, .. } => used.extend(args.iter().map(|arg| arg.value)),
            Terminator::Branch { condition, .. } => used.push(*condition),
            Terminator::Return { value } => used.push(value.value),
        }
        for value in used {
            if home.get(&value) != Some(&index) || params.contains(&value) {
                leaves_home.insert(value);
            }
        }
    }
    for block in &mut function.blocks {
        let mut settled = BTreeSet::new();
        let mut visit = |arg: &mut Arg| {
            if settled.insert(arg.value) && !leaves_home.contains(&arg.value) {
                arg.moved = true;
            }
        };
        match &mut block.terminator {
            Terminator::Jump { args, .. } => args.iter_mut().rev().for_each(&mut visit),
            Terminator::Branch { .. } => {}
            Terminator::Return { value } => visit(value),
        }
        for inst in block.insts.iter_mut().rev() {
            handed_over(&mut inst.op).into_iter().for_each(&mut visit);
        }
    }
    // A value read again after the use marked as its last (by an instruction that only reads it) is not moved.
    for block in &mut function.blocks {
        let mut read_later = BTreeSet::new();
        if let Terminator::Branch { condition, .. } = &block.terminator {
            read_later.insert(*condition);
        }
        for inst in block.insts.iter_mut().rev() {
            for arg in handed_over(&mut inst.op) {
                if read_later.contains(&arg.value) {
                    arg.moved = false;
                }
            }
            read_later.extend(uses(&inst.op));
        }
    }
}

struct Lowerer<'a> {
    roots: &'a RootLayouts,
    mode: &'a dyn Fn(&str) -> Option<CallHead>,
    stack: Vec<Builder>,
    functions: Vec<Option<StepFunction>>,
    source: u32,
}

type Lowered = Result<ValueId, StepRefusal>;

impl Lowerer<'_> {
    fn top(&mut self) -> &mut Builder {
        self.stack.last_mut().expect("a function is being built")
    }

    /// The value `name` is bound to in the function at `level`, capturing it from the enclosing functions.
    fn lookup(&mut self, level: usize, name: &str) -> Option<ValueId> {
        if let Some(value) = self.stack[level].local(name) {
            return Some(value);
        }
        if level == 0 {
            return None;
        }
        let outer = self.lookup(level - 1, name)?;
        let builder = &mut self.stack[level];
        let inner = builder.value();
        builder.params.push((name.to_string(), inner));
        builder.captured.push(outer);
        builder.scopes[0].push((name.to_string(), inner));
        Some(inner)
    }

    fn boxed(&mut self, op: Op, source: u32) -> ValueId {
        self.top().emit(Repr::Boxed, op, source)
    }

    fn truth(&mut self, value: ValueId, source: u32) -> ValueId {
        self.top().emit(Repr::Bool, Op::Truth { value }, source)
    }

    /// `condition` decides between two arms that each answer one value; the answer is the join block's parameter.
    fn choose(
        &mut self,
        condition: ValueId,
        then: impl FnOnce(&mut Self) -> Lowered,
        otherwise: impl FnOnce(&mut Self) -> Lowered,
    ) -> Lowered {
        let (then_block, else_block) = (self.top().block(Vec::new()), self.top().block(Vec::new()));
        let answer = self.top().value();
        let join = self.top().block(vec![(answer, Repr::Boxed)]);
        self.top().end(Terminator::Branch { condition, then: then_block, otherwise: else_block });
        self.top().current = then_block.0 as usize;
        let value = then(self)?;
        self.top().end(Terminator::Jump { to: join, args: vec![Arg::of(value)] });
        self.top().current = else_block.0 as usize;
        let value = otherwise(self)?;
        self.top().end(Terminator::Jump { to: join, args: vec![Arg::of(value)] });
        self.top().current = join.0 as usize;
        Ok(answer)
    }

    fn binding(&mut self, binding: &OirBinding, source: u32) -> Lowered {
        if let OirBindingRoot::Local(name) = &binding.root {
            let level = self.stack.len() - 1;
            if let Some(value) = self.lookup(level, name) {
                if binding.path.is_empty() {
                    return Ok(value);
                }
                let read = resolve_path(binding, &Repr::Boxed, &|_| None, &|_| None);
                return Ok(self.boxed(Op::Path { base: value, steps: read.steps }, source));
            }
        }
        let root = match &binding.root {
            OirBindingRoot::Entity => self.roots.entity.clone(),
            OirBindingRoot::Payload => self.roots.payload.clone(),
            OirBindingRoot::Config => self.roots.config.clone(),
            _ => None,
        }
        .unwrap_or(Repr::Boxed);
        let read = resolve_path(binding, &root, &|_| None, &|_| None);
        let typed = read.repr != Repr::Boxed;
        let value = self.top().emit(read.repr, Op::Read { root: binding.root.clone(), steps: read.steps }, source);
        Ok(if typed { self.boxed(Op::Box { value }, source) } else { value })
    }

    /// `body` as a function of this module taking `params`, with the values it captures from the enclosing function.
    fn lift(&mut self, params: &[String], body: &OirExpr) -> Result<(u32, Vec<Arg>), StepRefusal> {
        self.stack.push(Builder::new(params));
        let answer = self.expr(body);
        let mut builder = self.stack.pop().expect("the function being lifted");
        let value = answer?;
        builder.end(Terminator::Return { value: Arg::of(value) });
        let captures = builder.captured.iter().map(|outer| Arg::of(*outer)).collect();
        self.functions.push(Some(builder.finish()));
        Ok(((self.functions.len() - 1) as u32, captures))
    }

    fn call(&mut self, operator: &str, operands: &[&OirExpr], source: u32) -> Lowered {
        let lazy = match (self.mode)(operator) {
            Some(CallHead::Operator(CallMode { evaluation: OperatorEvaluation::Strict, .. })) => None,
            Some(CallHead::Operator(CallMode { evaluation: OperatorEvaluation::Site, lazy })) => Some(lazy),
            Some(CallHead::Operator(CallMode { evaluation: OperatorEvaluation::Form, .. })) => {
                return Err(StepRefusal::FormOperator(operator.to_string()))
            }
            Some(CallHead::Effect) => {
                let mut values = Vec::with_capacity(operands.len());
                for operand in operands {
                    values.push(Arg::of(self.expr(operand)?));
                }
                return Ok(self.boxed(Op::HostEffect { head: operator.to_string(), operands: values }, source));
            }
            None => return Err(StepRefusal::NoMode(operator.to_string())),
        };
        let Some(lazy) = lazy else {
            let mut args = Vec::with_capacity(operands.len());
            for operand in operands {
                args.push(Arg::of(self.expr(operand)?));
            }
            return Ok(self.boxed(Op::Call { operator: operator.to_string(), args }, source));
        };
        let mut site = Vec::with_capacity(operands.len());
        for (index, operand) in operands.iter().enumerate() {
            site.push(match operand {
                OirExpr::Lambda { params, body } => {
                    self.source += 1;
                    let (function, captures) = self.lift(params, body)?;
                    SiteOperand::Callable { function, captures }
                }
                operand if lazy.contains(&(index as u32)) => {
                    let (function, captures) = self.lift(&[], operand)?;
                    SiteOperand::Callable { function, captures }
                }
                operand => SiteOperand::Value {
                    literal: matches!(operand, OirExpr::String { value } if !value.starts_with('@')),
                    arg: Arg::of(self.expr(operand)?),
                },
            });
        }
        Ok(self.boxed(Op::SiteCall { operator: operator.to_string(), operands: site }, source))
    }

    fn form(&mut self, name: &str, args: &[OirExpr], source: u32) -> Option<Lowered> {
        let malformed = |what: &str| Some(Err(StepRefusal::Malformed(format!("{name}: {what}"))));
        Some(match name {
            "unquote" | "fn" => Err(StepRefusal::Quoted(name.to_string())),
            "do" => {
                let mut last = None;
                for arg in args {
                    last = Some(match self.expr(arg) {
                        Ok(value) => value,
                        Err(refusal) => return Some(Err(refusal)),
                    });
                }
                Ok(last.unwrap_or_else(|| self.boxed(Op::Null, source)))
            }
            "when" | "default" => {
                let [first, second, ..] = args else { return malformed("two operands") };
                let value = match self.expr(first) {
                    Ok(value) => value,
                    Err(refusal) => return Some(Err(refusal)),
                };
                if name == "when" {
                    let condition = self.truth(value, source);
                    self.choose(condition, |l| l.expr(second), |l| Ok(l.boxed(Op::Null, source)))
                } else {
                    let condition = self.top().emit(Repr::Bool, Op::IsNull { value }, source);
                    self.choose(condition, |l| l.expr(second), |_| Ok(value))
                }
            }
            "let" => {
                let [OirExpr::Array { items: pairs }, body] = args else { return malformed("a binding list and a body") };
                self.top().scopes.push(Vec::new());
                for pair in pairs {
                    let OirExpr::Array { items } = pair else { return malformed("each binding is a [name value] pair") };
                    let [OirExpr::String { value: bound }, initializer, ..] = items.as_slice() else {
                        return malformed("each binding is a [name value] pair");
                    };
                    let value = match self.expr(initializer) {
                        Ok(value) => value,
                        Err(refusal) => return Some(Err(refusal)),
                    };
                    self.top().scopes.last_mut().expect("the let scope").push((bound.clone(), value));
                }
                let answer = self.expr(body);
                self.top().scopes.pop();
                answer
            }
            _ => return None,
        })
    }

    fn expr(&mut self, expr: &OirExpr) -> Lowered {
        let source = self.source;
        self.source += 1;
        match expr {
            OirExpr::Null => Ok(self.boxed(Op::Null, source)),
            OirExpr::Bool { value } => Ok(self.boxed(Op::Bool { value: *value }, source)),
            OirExpr::Number { value } => Ok(self.boxed(Op::Number { value: *value }, source)),
            OirExpr::String { value } => Ok(self.boxed(Op::Text { value: value.clone() }, source)),
            OirExpr::Array { items } => {
                let mut values = Vec::with_capacity(items.len());
                for item in items {
                    values.push(Arg::of(self.expr(item)?));
                }
                Ok(self.boxed(Op::List { items: values }, source))
            }
            OirExpr::Object { fields } => {
                let mut values = Vec::with_capacity(fields.len());
                for (name, field) in fields {
                    values.push((name.clone(), Arg::of(self.expr(field)?)));
                }
                Ok(self.boxed(Op::Record { fields: values }, source))
            }
            OirExpr::Binding { binding } => self.binding(binding, source),
            OirExpr::BinaryOp { op: OirBinaryOp::And, left, right } => {
                let first = self.expr(left)?;
                let condition = self.truth(first, source);
                self.choose(
                    condition,
                    |l| {
                        let second = l.expr(right)?;
                        l.truth(second, source);
                        Ok(second)
                    },
                    |l| Ok(l.boxed(Op::Bool { value: false }, source)),
                )
            }
            OirExpr::BinaryOp { op: OirBinaryOp::Or, left, right } => {
                let first = self.expr(left)?;
                let condition = self.truth(first, source);
                self.choose(
                    condition,
                    |l| Ok(l.boxed(Op::Bool { value: true }, source)),
                    |l| {
                        let second = l.expr(right)?;
                        l.truth(second, source);
                        Ok(second)
                    },
                )
            }
            OirExpr::BinaryOp { op, left, right } => {
                let operator = match op {
                    OirBinaryOp::Eq => "=",
                    OirBinaryOp::Concat => "str/concat",
                    other => other.symbol(),
                };
                self.call(operator, &[left, right], source)
            }
            OirExpr::UnaryOp { op, operand } => {
                let operator = match op {
                    OirUnaryOp::Not => "not",
                    OirUnaryOp::Neg => "-",
                };
                self.call(operator, &[operand], source)
            }
            OirExpr::Conditional { condition, then_branch, else_branch } => {
                let decided = self.expr(condition)?;
                let condition = self.truth(decided, source);
                self.choose(condition, |l| l.expr(then_branch), |l| l.expr(else_branch))
            }
            OirExpr::Call { function, args } => match self.form(function, args, source) {
                Some(lowered) => lowered,
                None => self.call(function, &args.iter().collect::<Vec<_>>(), source),
            },
            OirExpr::Lambda { params, body } => {
                let (function, captures) = self.lift(params, body)?;
                Ok(self.boxed(Op::Closure { function, captures }, source))
            }
            OirExpr::Map { .. } => Err(StepRefusal::CollectionNode("map")),
            OirExpr::Filter { .. } => Err(StepRefusal::CollectionNode("filter")),
            OirExpr::Reduce { .. } => Err(StepRefusal::CollectionNode("reduce")),
            OirExpr::Effect { effect } => {
                let mut operands = Vec::new();
                for child in effect.child_exprs() {
                    operands.push(Arg::of(self.expr(child)?));
                }
                Ok(self.boxed(Op::Effect { effect: effect.clone(), operands }, source))
            }
        }
    }
}

/// `expr` as a step module: function 0 is the expression, the rest are its lifted functions. `mode` is how the
/// operator table says a call head takes its operands.
pub fn lower_step(
    expr: &OirExpr,
    roots: &RootLayouts,
    mode: &dyn Fn(&str) -> Option<CallHead>,
) -> Result<StepModule, StepRefusal> {
    let mut lowerer = Lowerer { roots, mode, stack: vec![Builder::new(&[])], functions: vec![None], source: 0 };
    let value = lowerer.expr(expr)?;
    let mut entry = lowerer.stack.pop().expect("the expression being built");
    entry.end(Terminator::Return { value: Arg::of(value) });
    lowerer.functions[0] = Some(entry.finish());
    Ok(StepModule { functions: lowerer.functions.into_iter().map(|f| f.expect("every function is built")).collect() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(name: &str) -> Option<CallHead> {
        let (evaluation, lazy) = match name {
            "theme/set" => return Some(CallHead::Effect),
            "quote" => (OperatorEvaluation::Site, vec![]),
            "+" | "-" | "*" | "=" | ">" | "not" | "str/concat" | "array/len" => (OperatorEvaluation::Strict, vec![]),
            "object/get" => (OperatorEvaluation::Site, vec![2]),
            "array/map" => (OperatorEvaluation::Site, vec![]),
            "tree/fold" => (OperatorEvaluation::Form, vec![]),
            _ => return None,
        };
        Some(CallHead::Operator(CallMode { evaluation, lazy }))
    }

    fn at(path: &str) -> OirExpr {
        OirExpr::binding(OirBinding::parse(path).expect("a binding"))
    }

    fn call(function: &str, args: Vec<OirExpr>) -> OirExpr {
        OirExpr::Call { function: function.to_string(), args }
    }

    fn text(expr: &OirExpr) -> String {
        lower_step(expr, &RootLayouts::default(), &mode).expect("lowers").to_string()
    }

    #[test]
    fn a_strict_call_evaluates_its_operands_then_calls_and_moves_what_it_last_uses() {
        let expr = OirExpr::add(at("@payload.a"), OirExpr::number(1.0));
        assert_eq!(
            text(&expr),
            "fn0() {\nb0:\n  %0: boxed = read @payload.a  ; 1\n  %1: boxed = 1  ; 2\n  %2: boxed = call +(move %0, move %1) !  ; 0\n  return move %2\n}\n"
        );
    }

    #[test]
    fn an_and_short_circuits_through_blocks() {
        let expr = OirExpr::and(at("@payload.a"), at("@payload.b"));
        assert_eq!(
            text(&expr),
            "fn0() {\nb0:\n  %0: boxed = read @payload.a  ; 1\n  %1: bool = truth %0 !  ; 0\n  branch %1, b1, b2\nb1:\n  %3: boxed = read @payload.b  ; 2\n  %4: bool = truth %3 !  ; 0\n  jump b3(move %3)\nb2:\n  %5: boxed = false  ; 0\n  jump b3(move %5)\nb3(%2: boxed):\n  return move %2\n}\n"
        );
    }

    #[test]
    fn a_let_binds_a_value_once_and_a_default_reads_its_fallback_only_when_null() {
        let expr = call(
            "let",
            vec![
                OirExpr::array(vec![OirExpr::array(vec![OirExpr::string("x"), at("@payload.a")])]),
                call("default", vec![at("@x"), OirExpr::number(0.0)]),
            ],
        );
        assert_eq!(
            text(&expr),
            "fn0() {\nb0:\n  %0: boxed = read @payload.a  ; 1\n  %1: bool = is-null %0  ; 2\n  branch %1, b1, b2\nb1:\n  %3: boxed = 0  ; 4\n  jump b3(move %3)\nb2:\n  jump b3(%0)\nb3(%2: boxed):\n  return move %2\n}\n"
        );
    }

    #[test]
    fn a_typed_root_is_read_by_slot_and_boxed_for_the_call() {
        use crate::ir::layout::{FieldSlot, RecordLayout};
        use crate::num_width::NumWidth;
        let payload = Repr::Record {
            layout: RecordLayout {
                fields: vec![
                    FieldSlot { name: "id".to_string(), repr: Repr::Text, required: true },
                    FieldSlot { name: "n".to_string(), repr: Repr::Int { width: NumWidth::I64 }, required: true },
                ],
            },
        };
        let roots = RootLayouts { payload: Some(payload), ..Default::default() };
        let expr = OirExpr::add(at("@payload.n"), OirExpr::number(1.0));
        let module = lower_step(&expr, &roots, &mode).expect("lowers");
        assert_eq!(
            module.to_string(),
            "fn0() {\nb0:\n  %0: int = read @payload.#1  ; 1\n  %1: boxed = box %0  ; 1\n  %2: boxed = 1  ; 2\n  %3: boxed = call +(move %1, move %2) !  ; 0\n  return move %3\n}\n"
        );
    }

    #[test]
    fn a_lambda_is_lifted_and_captures_what_it_reads_from_outside() {
        let expr = call(
            "let",
            vec![
                OirExpr::array(vec![OirExpr::array(vec![OirExpr::string("k"), OirExpr::number(2.0)])]),
                OirExpr::lambda(vec!["x".to_string()], OirExpr::add(at("@x"), at("@k"))),
            ],
        );
        assert_eq!(
            text(&expr),
            "fn0() {\nb0:\n  %0: boxed = 2  ; 1\n  %1: boxed = closure fn1[move %0]  ; 2\n  return move %1\n}\nfn1(x %0, capture k %1) {\nb0:\n  %2: boxed = call +(%0, %1) !  ; 3\n  return move %2\n}\n"
        );
    }

    #[test]
    fn a_literal_key_is_marked_and_a_lazy_default_is_a_function_the_operator_runs() {
        let expr = call(
            "object/get",
            vec![at("@payload.o"), OirExpr::string("a.b"), OirExpr::add(at("@payload.n"), OirExpr::number(1.0))],
        );
        assert_eq!(
            text(&expr),
            "fn0() {\nb0:\n  %0: boxed = read @payload.o  ; 1\n  %1: boxed = \"a.b\"  ; 2\n  %2: boxed = call-site object/get(move %0, literal move %1, fn1[]) !  ; 0\n  return move %2\n}\nfn1() {\nb0:\n  %0: boxed = read @payload.n  ; 4\n  %1: boxed = 1  ; 5\n  %2: boxed = call +(move %0, move %1) !  ; 3\n  return move %2\n}\n"
        );
    }

    #[test]
    fn control_a_computed_key_is_not_marked_literal() {
        let expr = call("object/get", vec![at("@payload.o"), at("@payload.k")]);
        assert_eq!(
            text(&expr),
            "fn0() {\nb0:\n  %0: boxed = read @payload.o  ; 1\n  %1: boxed = read @payload.k  ; 2\n  %2: boxed = call-site object/get(move %0, move %1) !  ; 0\n  return move %2\n}\n"
        );
    }

    #[test]
    fn a_lambda_operand_is_a_function_the_operator_runs() {
        let expr = call(
            "array/map",
            vec![at("@payload.xs"), OirExpr::lambda(vec!["x".to_string()], OirExpr::add(at("@x"), at("@payload.n")))],
        );
        assert_eq!(
            text(&expr),
            "fn0() {\nb0:\n  %0: boxed = read @payload.xs  ; 1\n  %1: boxed = call-site array/map(move %0, fn1[]) !  ; 0\n  return move %1\n}\nfn1(x %0) {\nb0:\n  %1: boxed = read @payload.n  ; 5\n  %2: boxed = call +(%0, move %1) !  ; 3\n  return move %2\n}\n"
        );
    }

    #[test]
    fn a_quote_keeps_its_encoded_body_as_a_literal_operand() {
        assert_eq!(
            text(&call("quote", vec![OirExpr::string("[1]")])),
            "fn0() {\nb0:\n  %0: boxed = \"[1]\"  ; 1\n  %1: boxed = call-site quote(literal move %0) !  ; 0\n  return move %1\n}\n"
        );
    }

    #[test]
    fn a_table_effect_left_as_a_call_is_an_effect_on_its_operand_values() {
        assert_eq!(
            text(&call("theme/set", vec![at("@payload.k")])),
            "fn0() {\nb0:\n  %0: boxed = read @payload.k  ; 1\n  %1: boxed = host-effect theme/set(move %0) !  ; 0\n  return move %1\n}\n"
        );
    }

    #[test]
    fn what_has_no_value_entry_is_refused_by_name() {
        let roots = RootLayouts::default();
        let refused = |expr: OirExpr| lower_step(&expr, &roots, &mode).expect_err("refused");
        assert_eq!(refused(call("tree/fold", vec![at("@payload.o")])), StepRefusal::FormOperator("tree/fold".to_string()));
        assert_eq!(refused(call("unquote", vec![OirExpr::number(1.0)])), StepRefusal::Quoted("unquote".to_string()));
        assert_eq!(refused(call("agent/generate", vec![])), StepRefusal::NoMode("agent/generate".to_string()));
        assert_eq!(refused(call("let", vec![OirExpr::number(1.0)])), StepRefusal::Malformed("let: a binding list and a body".to_string()));
    }

    #[test]
    fn control_a_strict_operator_of_the_same_shape_lowers() {
        assert!(lower_step(&call("array/len", vec![at("@payload.o")]), &RootLayouts::default(), &mode).is_ok());
    }
}
