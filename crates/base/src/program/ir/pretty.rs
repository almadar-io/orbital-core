//! OIR Pretty Printer
//!
//! Human-readable formatting for OIR structures.
#[cfg(not(feature = "std"))]
use std::prelude::v1::*;

use std::fmt::Write;

use crate::ir::effects::OirEffect;
use crate::ir::expr::{OirBinaryOp, OirBinding, OirBindingRoot, OirExpr, OirUnaryOp};
use crate::ir::types::{
    OirEntity, OirField, OirModule, OirPage, OirPersistence, OirState, OirTrait, OirTransition,
    OirTransitionFrom, OirType,
};

/// Pretty print configuration
#[derive(Debug, Clone)]
pub struct PrettyConfig {
    /// Indentation string (default: 2 spaces)
    pub indent: String,
    /// Show descriptions
    pub show_descriptions: bool,
    /// Show metadata
    pub show_metadata: bool,
    /// Maximum items before truncating
    pub max_items: Option<usize>,
}

impl Default for PrettyConfig {
    fn default() -> Self {
        Self {
            indent: "  ".to_string(),
            show_descriptions: true,
            show_metadata: false,
            max_items: None,
        }
    }
}

/// Pretty printer for OIR
pub struct PrettyPrinter {
    config: PrettyConfig,
    output: String,
    depth: usize,
}

impl PrettyPrinter {
    /// Create a new pretty printer
    pub fn new() -> Self {
        Self::with_config(PrettyConfig::default())
    }

    /// Create with custom config
    pub fn with_config(config: PrettyConfig) -> Self {
        Self {
            config,
            output: String::new(),
            depth: 0,
        }
    }

    /// Print a module
    pub fn print_module(mut self, module: &OirModule) -> String {
        self.write_module(module);
        self.output
    }

    /// Print an entity
    pub fn print_entity(mut self, entity: &OirEntity) -> String {
        self.write_entity(entity);
        self.output
    }

    /// Print a trait
    pub fn print_trait(mut self, trait_def: &OirTrait) -> String {
        self.write_trait(trait_def);
        self.output
    }

    /// Print a page
    pub fn print_page(mut self, page: &OirPage) -> String {
        self.write_page(page);
        self.output
    }

    /// Print an expression
    pub fn print_expr(mut self, expr: &OirExpr) -> String {
        self.write_expr(expr);
        self.output
    }

    // Internal helpers

    fn indent(&self) -> String {
        self.config.indent.repeat(self.depth)
    }

    fn writeln(&mut self, s: &str) {
        writeln!(self.output, "{}{}", self.indent(), s).unwrap();
    }

    fn write(&mut self, s: &str) {
        write!(self.output, "{}", s).unwrap();
    }
}

impl Default for PrettyPrinter {
    fn default() -> Self {
        Self::new()
    }
}

/// Print a module to a string
pub fn pretty_print(module: &OirModule) -> String {
    PrettyPrinter::new().print_module(module)
}

/// Print a module with custom config
pub fn pretty_print_with_config(module: &OirModule, config: PrettyConfig) -> String {
    PrettyPrinter::with_config(config).print_module(module)
}

/// Print an expression to a string
pub fn pretty_print_expr(expr: &OirExpr) -> String {
    PrettyPrinter::new().print_expr(expr)
}

mod decls;
mod exprs;
mod effect_writer;

#[cfg(test)]
mod tests;
