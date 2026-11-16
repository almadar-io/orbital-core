//! Proof of the step IR (P-039): a runner that executes a step function by calling the operators' own value entries,
//! compared with the evaluator on the same expression and bindings. It is a test harness, not an engine: nothing in
//! the kernel calls it. Effects and free-standing closures are lowered but not run here.

use orbital_core_base::error::OrbitalError;
use orbital_core_base::ir::expr::OirBinding;
use orbital_core_base::ir::layout::PathStep;
use orbital_core_base::ir::step::lower::{CallMode, RootLayouts};
use orbital_core_base::ir::step::{lower_step, Arg, Op, SiteOperand, StepModule, StepRefusal, Terminator, ValueId};
use orbital_core_base::json_value::from_json_value;
use orbital_core_base::language::registry::operators;
use orbital_core_base::schema::sexpr::SExpression;
use orbital_core_base::value::{value_map_with_capacity, Value};
use orbital_core_eval::evaluator::context::{BindingContext, BindingContextBuilder};
use orbital_core_eval::evaluator::operators::{eval_expr, get_operator, CallSite, EvalResult};
use orbital_core_eval::language_loader::preflight;
use orbital_core_eval::lower::lower_sexpr;
use std::sync::Mutex;
use std::collections::BTreeMap;

/// The step module of `expr`, by the one lowering and the operator table's evaluation modes.
pub fn step_of(expr: &SExpression) -> Result<StepModule, String> {
    let lowered = lower_sexpr(expr).map_err(|error| format!("lowering: {error}"))?;
    let mode = |name: &str| {
        let operator = operators().get(name)?;
        Some(CallMode { evaluation: operator.evaluation?, lazy: operator.lazy_operands.clone() })
    };
    lower_step(&lowered, &RootLayouts::default(), &mode).map_err(|refusal| refusal_name(&refusal))
}

pub fn refusal_name(refusal: &StepRefusal) -> String {
    match refusal {
        StepRefusal::Quoted(form) => format!("quoted code: {form}"),
        StepRefusal::FormOperator(name) => format!("form operator: {name}"),
        StepRefusal::NoMode(name) => format!("no evaluation mode: {name}"),
        StepRefusal::CollectionNode(kind) => format!("collection node: {kind}"),
        StepRefusal::Malformed(what) => format!("malformed: {what}"),
    }
}

fn path_text(steps: &[PathStep]) -> Vec<String> {
    steps
        .iter()
        .map(|step| match step {
            PathStep::Key { key } => key.clone(),
            PathStep::Index { index } => index.to_string(),
            PathStep::Slot { slot } => panic!("the runner reads dynamic roots, found slot {slot}"),
            PathStep::Length => "length".to_string(),
        })
        .collect()
}

/// Why a run ended without a value: the program raised, or the module holds something this harness does not run.
enum Stop {
    Raised(OrbitalError),
    NotRun(String),
}

struct Machine<'a> {
    module: &'a StepModule,
    ctx: &'a BindingContext,
    not_run: Mutex<Option<String>>,
}

/// The call site of one site call: its literal marks, and each callable as the function and captured values to run.
struct Site<'a> {
    machine: &'a Machine<'a>,
    literal: Vec<bool>,
    callables: BTreeMap<usize, (u32, Vec<Value>)>,
}

impl CallSite for Site<'_> {
    fn literal(&self, index: usize) -> bool {
        self.literal.get(index).copied().unwrap_or(false)
    }

    fn invoke(&self, index: usize, values: Vec<Value>) -> EvalResult {
        let Some((function, captures)) = self.callables.get(&index) else {
            *self.machine.not_run.lock().expect("the flag") = Some(format!("operand {index} is not a callable"));
            return Ok(Value::Null);
        };
        match self.machine.function(*function, values, captures.clone()) {
            Ok(value) => Ok(value),
            Err(Stop::Raised(error)) => Err(error),
            Err(Stop::NotRun(what)) => {
                *self.machine.not_run.lock().expect("the flag") = Some(what);
                Ok(Value::Null)
            }
        }
    }
}

impl Machine<'_> {
    fn function(&self, index: u32, given: Vec<Value>, captures: Vec<Value>) -> Result<Value, Stop> {
        let function = &self.module.functions[index as usize];
        let own = function.params.len() - function.captures as usize;
        let mut values: BTreeMap<ValueId, Value> = BTreeMap::new();
        let mut given = given.into_iter();
        for (_, param) in &function.params[..own] {
            values.insert(*param, given.next().unwrap_or(Value::Null));
        }
        for ((_, param), value) in function.params[own..].iter().zip(captures) {
            values.insert(*param, value);
        }
        let take = |values: &mut BTreeMap<ValueId, Value>, arg: &Arg| {
            if arg.moved {
                values.remove(&arg.value).expect("a moved value is held")
            } else {
                values[&arg.value].clone()
            }
        };
        let mut at = 0usize;
        loop {
            let block = &function.blocks[at];
            for inst in &block.insts {
                let value = match &inst.op {
                    Op::Null => Value::Null,
                    Op::Bool { value } => Value::Bool(*value),
                    Op::Number { value } => Value::Number(*value),
                    Op::Text { value } => Value::string(value.clone()),
                    Op::Read { root, steps } => {
                        let binding = OirBinding::new(root.clone(), path_text(steps)).to_string();
                        match self.ctx.resolve_binding(&binding) {
                            Some(value) => value,
                            None if binding.starts_with("@trait.") => Value::string(binding),
                            None => Value::Null,
                        }
                    }
                    Op::Path { base, steps } => values[base].get_path_owned(&path_text(steps).join(".")).unwrap_or(Value::Null),
                    Op::Box { value } => values[value].clone(),
                    Op::Truth { value } => Value::Bool(values[value].boolean().map_err(|error| Stop::Raised(error.into()))?),
                    Op::IsNull { value } => Value::Bool(values[value].is_null()),
                    Op::Call { operator, args } => {
                        let mut operands: Vec<Value> = args.iter().map(|arg| take(&mut values, arg)).collect();
                        let site = Site { machine: self, literal: Vec::new(), callables: BTreeMap::new() };
                        self.operator(operator, &mut operands, &site)?
                    }
                    Op::SiteCall { operator, operands } => {
                        let mut site = Site { machine: self, literal: Vec::new(), callables: BTreeMap::new() };
                        let mut evaluated = Vec::with_capacity(operands.len());
                        for (index, operand) in operands.iter().enumerate() {
                            match operand {
                                SiteOperand::Value { arg, literal } => {
                                    site.literal.push(*literal);
                                    evaluated.push(take(&mut values, arg));
                                }
                                SiteOperand::Callable { function, captures } => {
                                    site.literal.push(false);
                                    let held = captures.iter().map(|arg| take(&mut values, arg)).collect();
                                    site.callables.insert(index, (*function, held));
                                    evaluated.push(Value::Null);
                                }
                            }
                        }
                        self.operator(operator, &mut evaluated, &site)?
                    }
                    Op::List { items } => Value::array(items.iter().map(|arg| take(&mut values, arg)).collect::<Vec<_>>()),
                    Op::Record { fields } => {
                        let mut record = value_map_with_capacity(fields.len());
                        for (name, arg) in fields {
                            record.insert(name.as_str().into(), take(&mut values, arg));
                        }
                        Value::object(record)
                    }
                    Op::Effect { effect, .. } => return Err(Stop::NotRun(format!("effect {}", effect.kind()))),
                    Op::Closure { .. } => return Err(Stop::NotRun("closure value".to_string())),
                };
                values.insert(inst.result, value);
            }
            match &block.terminator {
                Terminator::Return { value } => return Ok(take(&mut values, value)),
                Terminator::Branch { condition, then, otherwise } => {
                    at = if values[condition] == Value::Bool(true) { then.0 } else { otherwise.0 } as usize;
                }
                Terminator::Jump { to, args } => {
                    let passed: Vec<Value> = args.iter().map(|arg| take(&mut values, arg)).collect();
                    at = to.0 as usize;
                    for ((param, _), value) in function.blocks[at].params.iter().zip(passed) {
                        values.insert(*param, value);
                    }
                }
            }
        }
    }

    fn operator(&self, name: &str, operands: &mut [Value], site: &Site<'_>) -> Result<Value, Stop> {
        let kind = get_operator(name).ok_or_else(|| Stop::NotRun(format!("unknown operator {name}")))?;
        let answer = kind
            .apply_values(operands, site, self.ctx)
            .ok_or_else(|| Stop::NotRun(format!("{name} has no value entry")))?;
        if let Some(what) = self.not_run.lock().expect("the flag").take() {
            return Err(Stop::NotRun(what));
        }
        answer.map_err(Stop::Raised)
    }
}

/// What running function 0 of `module` answers: the value or the evaluation error; `Err(what)` when the module holds
/// something this harness does not run.
pub fn run(module: &StepModule, ctx: &BindingContext) -> Result<Result<Value, String>, String> {
    let machine = Machine { module, ctx, not_run: Mutex::new(None) };
    match machine.function(0, Vec::new(), Vec::new()) {
        Ok(value) => Ok(Ok(value)),
        Err(Stop::Raised(error)) => Ok(Err(error.to_string())),
        Err(Stop::NotRun(what)) => Err(what),
    }
}

pub fn evaluated(expr: &SExpression, ctx: &BindingContext) -> Result<Value, String> {
    eval_expr(expr, ctx).map_err(|error| error.to_string())
}

fn agree(source: serde_json::Value, payload: serde_json::Value) {
    preflight().expect("the installed language data must load");
    let expr: SExpression = from_json_value(&source).expect("an expression");
    let ctx = BindingContextBuilder::new().payload(from_json_value::<Value>(&payload).expect("a payload")).build();
    let module = step_of(&expr).unwrap_or_else(|refusal| panic!("{source} is refused: {refusal}"));
    let ran = run(&module, &ctx).unwrap_or_else(|what| panic!("{source} is not runnable: {what}"));
    assert_eq!(ran, evaluated(&expr, &ctx), "{source}\n{module}");
}

#[test]
fn arithmetic_and_comparison_agree_with_the_evaluator() {
    let payload = serde_json::json!({ "a": 7, "b": 2.5, "name": "ada", "xs": [1, 2, 3] });
    agree(serde_json::json!(["+", "@payload.a", 1, ["*", "@payload.b", 2]]), payload.clone());
    agree(serde_json::json!(["<=", ["array/len", "@payload.xs"], "@payload.a"]), payload.clone());
    agree(serde_json::json!(["str/concat", "@payload.name", "-", ["str/upper", "@payload.name"]]), payload.clone());
    agree(serde_json::json!({ "n": ["-", "@payload.a"], "list": ["@payload.a", ["%", "@payload.a", 4]] }), payload);
}

#[test]
fn control_forms_agree_with_the_evaluator_on_both_arms() {
    for ok in [true, false] {
        let payload = serde_json::json!({ "ok": ok, "n": 3, "missing": null });
        agree(serde_json::json!(["if", "@payload.ok", ["+", "@payload.n", 1], "no"]), payload.clone());
        agree(serde_json::json!(["when", "@payload.ok", "yes"]), payload.clone());
        agree(serde_json::json!(["and", "@payload.ok", [">", "@payload.n", 2], ["=", "@payload.n", 3]]), payload.clone());
        agree(serde_json::json!(["or", "@payload.ok", ["<", "@payload.n", 2], ["=", "@payload.n", 3]]), payload.clone());
        agree(serde_json::json!(["default", "@payload.missing", ["*", "@payload.n", 2]]), payload.clone());
        agree(serde_json::json!(["default", "@payload.n", "unused"]), payload.clone());
        agree(serde_json::json!(["let", [["x", ["+", "@payload.n", 1]], ["y", ["*", "@x", 2]]], ["do", "@x", ["+", "@x", "@y"]]]), payload);
    }
}

#[test]
fn a_failure_is_the_evaluators_failure() {
    let payload = serde_json::json!({ "text": "ada", "n": 3 });
    agree(serde_json::json!(["+", "@payload.text", 1]), payload.clone());
    agree(serde_json::json!(["and", "@payload.n", true]), payload.clone());
    agree(serde_json::json!(["if", "@payload.text", 1, 2]), payload);
}

#[test]
fn control_a_short_circuit_does_not_evaluate_the_arm_it_skips() {
    let payload = serde_json::json!({ "text": "ada" });
    agree(serde_json::json!(["or", true, ["+", "@payload.text", 1]]), payload.clone());
    agree(serde_json::json!(["and", false, ["+", "@payload.text", 1]]), payload.clone());
    agree(serde_json::json!(["if", true, 1, ["+", "@payload.text", 1]]), payload);
}

#[test]
fn a_literal_key_is_a_dotted_path_and_a_computed_key_is_one_whole_key() {
    let payload = serde_json::json!({ "o": { "a": { "b": 1 }, "a.b": 2 }, "k": "a.b", "xs": [10, 20] });
    agree(serde_json::json!(["object/get", "@payload.o", "a.b"]), payload.clone());
    agree(serde_json::json!(["object/get", "@payload.o", "@payload.k"]), payload.clone());
    agree(serde_json::json!(["object/has", "@payload.o", "a.b"]), payload.clone());
    agree(serde_json::json!(["object/has", "@payload.o", "@payload.k"]), payload.clone());
    agree(serde_json::json!(["object/has", "@payload.o", "a.c"]), payload.clone());
    agree(serde_json::json!(["object/get", "@payload.xs", 1]), payload.clone());
    agree(serde_json::json!(["object/set", "@payload.o", "a.c", 3]), payload.clone());
    agree(serde_json::json!(["object/set", "@payload.o", "@payload.k", 3]), payload);
}

#[test]
fn a_default_is_evaluated_only_when_nothing_is_found() {
    let payload = serde_json::json!({ "o": { "a": 1, "n": null }, "text": "ada" });
    agree(serde_json::json!(["object/get", "@payload.o", "missing", ["str/upper", "@payload.text"]]), payload.clone());
    agree(serde_json::json!(["object/get", "@payload.o", "a", ["+", "@payload.text", 1]]), payload.clone());
    agree(serde_json::json!(["object/get", "@payload.o", "n", "unused"]), payload.clone());
    agree(serde_json::json!(["object/get", "@payload.o", "missing", ["+", "@payload.text", 1]]), payload.clone());
    agree(serde_json::json!(["object/get", "@payload.text", "a", 1]), payload);
}

#[test]
fn a_callback_runs_as_a_lifted_function_with_its_captures() {
    let payload = serde_json::json!({ "xs": [1, 2, 3, 4], "n": 2, "rows": [{ "v": 1 }, { "v": 5 }], "text": "ada" });
    agree(serde_json::json!(["array/map", "@payload.xs", ["fn", "x", ["*", "@x", "@payload.n"]]]), payload.clone());
    agree(serde_json::json!(["array/map", "@payload.xs", ["fn", ["x", "i"], ["+", "@x", "@i"]]]), payload.clone());
    agree(serde_json::json!(["array/filter", "@payload.rows", ["fn", "row", [">", "@item.v", "@payload.n"]]]), payload.clone());
    agree(serde_json::json!(["array/find", "@payload.xs", ["fn", "x", [">", "@x", "@payload.n"]]]), payload.clone());
    agree(serde_json::json!(["array/find", "@payload.xs", ["fn", "x", [">", "@x", 9]]]), payload.clone());
    agree(serde_json::json!(["array/some", "@payload.xs", ["fn", "x", ["=", "@x", 3]]]), payload.clone());
    agree(serde_json::json!(["array/every", "@payload.xs", ["fn", "x", ["<", "@x", 3]]]), payload.clone());
    agree(
        serde_json::json!(["let", [["k", 10]], ["array/map", "@payload.xs", ["fn", "x", ["+", "@x", "@k", "@index"]]]]),
        payload.clone(),
    );
    agree(serde_json::json!(["array/map", "@payload.xs", ["fn", "x", ["+", "@x", "@payload.text"]]]), payload.clone());
    agree(serde_json::json!(["array/reduce", "@payload.xs", 0, ["fn", ["acc", "x"], ["+", "@acc", "@x", "@payload.n"]]]), payload.clone());
    agree(serde_json::json!(["array/reduce", "@payload.xs", [], ["fn", ["acc", "x"], ["array/append", "@acc", ["*", "@x", 2]]]]), payload.clone());
    agree(serde_json::json!(["array/reduce", "@payload.xs", 0, ["fn", ["acc", "x"], ["+", "@acc", "@payload.text"]]]), payload.clone());
    agree(serde_json::json!(["array/findIndex", "@payload.xs", ["fn", "x", [">", "@x", "@payload.n"]]]), payload.clone());
    agree(serde_json::json!(["array/findIndex", "@payload.xs", ["fn", ["x", "i"], ["=", "@i", 9]]]), payload.clone());
    agree(serde_json::json!(["array/filter", "@payload.xs", ["fn", "x", "@x"]]), payload.clone());
    agree(serde_json::json!(["array/map", "@payload.text", ["fn", "x", "@x"]]), payload);
}

#[test]
fn a_shape_written_as_a_literal_reaches_the_reader() {
    let payload = serde_json::json!({ "text": "{\"a\":1}", "bad": "{", "n": 3, "shape": "number" });
    agree(serde_json::json!(["json/parse", "@payload.text", "number"]), payload.clone());
    agree(serde_json::json!(["json/parse", "@payload.bad", "number"]), payload.clone());
    agree(serde_json::json!(["as", "@payload.n", "number"]), payload.clone());
    agree(serde_json::json!(["as", "@payload.text", "number"]), payload.clone());
    agree(serde_json::json!(["as", "@payload.text", "CallKey"]), payload);
}

#[test]
fn what_cannot_be_called_on_values_is_refused_by_name() {
    preflight().expect("the installed language data must load");
    let refused = |source: serde_json::Value| {
        let expr: SExpression = from_json_value(&source).expect("an expression");
        step_of(&expr).expect_err("refused")
    };
    assert_eq!(refused(serde_json::json!(["tree/fold", "@payload.o", {}, "a", "b"])), "form operator: tree/fold");
    assert_eq!(refused(serde_json::json!(["quote", ["+", 1, 2]])), "quoted code: quote");
}
