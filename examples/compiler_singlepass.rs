//! A Wasm module can be compiled with multiple compilers.
//!
//! This example illustrates how to use the Singlepass compiler.
//!
//! You can run the example directly by executing in Wasmer root:
//!
//! ```shell
//! cargo run --example compiler-singlepass --release --features "singlepass"
//! ```
//!
//! Ready?

use wasmer::{imports, wat2wasm, Instance, Module, Store, Value};
use wasmer_compiler_singlepass::Singlepass;
use wasmer_engine_universal::Universal;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Let's declare the Wasm module with the text representation.
    let wasm_bytes = wat2wasm(
        r#"
(module
  (type $sum_t (func (param i32 i32) (result i32)))
  (func $sum_f (type $sum_t) (param $x i32) (param $y i32) (result i32)
    local.get $x
    local.get $y
    i32.add)
  (export "sum" (func $sum_f)))
"#
        .as_bytes(),
    )?;

    // Use Singlepass compiler with the default settings
    let compiler = Singlepass::default();

    // Create the store
    let store = Store::new(&Universal::new(compiler).engine());

    println!("Compiling module...");
    // Let's compile the Wasm module.
    let module = Module::new(&store, wasm_bytes)?;

    // Create an empty import object.
    let import_object = imports! {};

    println!("Instantiating module...");
    // Let's instantiate the Wasm module.
    let instance = Instance::new(&module, &import_object)?;

    let sum = instance.lookup_function("sum").expect("function lookup");

    println!("Calling `sum` function...");
    // Let's call the `sum` exported function. The parameters are a
    // slice of `Value`s. The results are a boxed slice of `Value`s.
    let results = sum.call(&[Value::I32(1), Value::I32(2)])?;

    println!("Results: {:?}", results);
    assert_eq!(results.to_vec(), vec![Value::I32(3)]);

    Ok(())
}

#[test]
#[cfg(feature = "singlepass")]
fn test_compiler_singlepass() -> Result<(), Box<dyn std::error::Error>> {
    main()
}
