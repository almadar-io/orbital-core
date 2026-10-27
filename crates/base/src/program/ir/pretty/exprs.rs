//! Writing expressions, bindings and operators.

use super::*;

impl PrettyPrinter {
    /// `exprs` separated by `, `.
    pub(super) fn write_exprs(&mut self, exprs: &[OirExpr]) {
        for (i, expr) in exprs.iter().enumerate() {
            if i > 0 {
                self.write(", ");
            }
            self.write_expr(expr);
        }
    }

    pub(super) fn write_expr(&mut self, expr: &OirExpr) {
        match expr {
            OirExpr::Null => self.write("null"),
            OirExpr::Bool { value } => self.write(&value.to_string()),
            OirExpr::Number { value } => self.write(&value.to_string()),
            OirExpr::String { value } => self.write(&format!("\"{}\"", value)),
            OirExpr::Array { items } => {
                self.write("[");
                self.write_exprs(items);
                self.write("]");
            }
            OirExpr::Object { fields } => {
                self.write("{ ");
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        self.write(", ");
                    }
                    self.write(&format!("{}: ", k));
                    self.write_expr(v);
                }
                self.write(" }");
            }
            OirExpr::Binding { binding } => {
                self.write_binding(binding);
            }
            OirExpr::BinaryOp { op, left, right } => {
                self.write("(");
                self.write_expr(left);
                self.write(&format!(" {} ", self.format_binary_op(op)));
                self.write_expr(right);
                self.write(")");
            }
            OirExpr::UnaryOp { op, operand } => {
                self.write(&self.format_unary_op(op));
                self.write_expr(operand);
            }
            OirExpr::Conditional {
                condition,
                then_branch,
                else_branch,
            } => {
                self.write("if ");
                self.write_expr(condition);
                self.write(" then ");
                self.write_expr(then_branch);
                self.write(" else ");
                self.write_expr(else_branch);
            }
            OirExpr::Call { function, args } => {
                self.write(&format!("{}(", function));
                self.write_exprs(args);
                self.write(")");
            }
            OirExpr::Lambda { params, body } => {
                self.write(&format!("({}) => ", params.join(", ")));
                self.write_expr(body);
            }
            OirExpr::Map {
                source,
                variable,
                body,
            } => {
                self.write("map(");
                self.write_expr(source);
                self.write(&format!(", {} => ", variable));
                self.write_expr(body);
                self.write(")");
            }
            OirExpr::Filter {
                source,
                variable,
                predicate,
            } => {
                self.write("filter(");
                self.write_expr(source);
                self.write(&format!(", {} => ", variable));
                self.write_expr(predicate);
                self.write(")");
            }
            OirExpr::Reduce {
                source,
                variable,
                accumulator,
                initial,
                body,
            } => {
                self.write("reduce(");
                self.write_expr(source);
                self.write(&format!(", {}, {} => ", variable, accumulator));
                self.write_expr(body);
                self.write(", initial: ");
                self.write_expr(initial);
                self.write(")");
            }
            OirExpr::Effect { effect } => {
                self.write_effect(effect);
            }
        }
    }

    pub(super) fn write_binding(&mut self, binding: &OirBinding) {
        // `@trait.<Name>[.<slot>]` pretty-prints with the trait name as the
        // second segment, followed by an optional slot. Other roots use their
        // bare name.
        if let OirBindingRoot::Trait(trait_name) = &binding.root {
            if binding.path.is_empty() {
                self.write(&format!("@trait.{}", trait_name));
            } else {
                self.write(&format!("@trait.{}.{}", trait_name, binding.path.join(".")));
            }
            return;
        }

        let root = match &binding.root {
            OirBindingRoot::Entity => "@entity",
            OirBindingRoot::Payload => "@payload",
            OirBindingRoot::State => "@state",
            OirBindingRoot::Config => "@config",
            OirBindingRoot::User => "@user",
            OirBindingRoot::Pages => "@pages",
            OirBindingRoot::CurrentTheme => "@currentTheme",
            OirBindingRoot::Locale => "@locale",
            OirBindingRoot::Event => "@event",
            OirBindingRoot::PrevEvents => "@prevEvents",
            OirBindingRoot::PrevStates => "@prevStates",
            OirBindingRoot::FromState => "@fromState",
            OirBindingRoot::ToState => "@toState",
            OirBindingRoot::Trait(_) => unreachable!("handled above"),
            OirBindingRoot::Local(name) => name,
        };

        if binding.path.is_empty() {
            self.write(root);
        } else {
            self.write(&format!("{}.{}", root, binding.path.join(".")));
        }
    }

    pub(super) fn format_binary_op(&self, op: &OirBinaryOp) -> &'static str {
        op.symbol()
    }

    pub(super) fn format_unary_op(&self, op: &OirUnaryOp) -> String {
        op.symbol().to_string()
    }
}
