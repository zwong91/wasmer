#[test]
#[ignore]
fn test_test_dlmalloc_partial_2() {
    assert_emscripten_output!(
        "../emscripten_resources/emtests/test_dlmalloc_partial_2.wasm",
        "test_dlmalloc_partial_2",
        vec![],
        "../emscripten_resources/emtests/test_dlmalloc_partial_2.out"
    );
}