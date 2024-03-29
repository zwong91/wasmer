//! This test suite does all the tests that involve any compiler
//! implementation, such as: singlepass, cranelift or llvm depending
//! on what's available on the target.

#[macro_use]
extern crate compiler_test_derive;

mod config;
mod deterministic;
mod fast_gas_metering;
mod imports;
mod issues;
// mod multi_value_imports;
mod compilation;
mod native_functions;
mod serialize;
mod stack_limiter;
mod traps;
mod wast;

pub use crate::config::{Compiler, Config, Engine};
pub use crate::wast::run_wast;
