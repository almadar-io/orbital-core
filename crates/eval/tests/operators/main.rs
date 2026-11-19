//! Operator behavior: shared vectors, arity, types and the evaluator's edges.

#[global_allocator]
static ALLOC: orbital_core_base::alloc_meter::CountingAlloc<std::alloc::System> =
    orbital_core_base::alloc_meter::CountingAlloc::thread_only(std::alloc::System);

mod array_sum_avg_key;
mod behavior_apply;
mod behavior_ref_value;
mod binding_token_contract;
mod codec_arity;
mod codec_operators;
mod color_operators;
mod evaluation_mode;
mod json_valid_operator;
mod lambda_operators;
mod linear_fold;
mod let_scope_contract;
mod lower_call_head_contract;
mod lowering_corpus;
mod math_log_edges;
mod no_generic_types;
mod now_stamp_operators;
mod number_operators;
mod parallel_operators;
mod object_get_default;
mod operator_types;
mod quasiquote;
mod quote_eval;
mod step_runner;
mod time_parse_operator;
mod time_unit_parity;
mod xml_operators;
mod xml_valid_operator;
mod yaml_operators;
