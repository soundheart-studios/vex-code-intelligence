//! Objective-C cases found by diffing real repositories against Cortex.

use vex_extract::engine::parse_ast;

/// facebookarchive/pop's POPVector.h: a C++ `operator[](size_t i)` read by
/// the Objective-C grammar is a call_expression whose name is empty.
/// Cortex's `if name:` drops it; it must not become a call or a ref.
#[test]
fn a_call_with_an_empty_name_is_not_a_call() {
    let src = b"#import <objc/NSObjCRuntime.h>\n\
namespace POP {\n\
  template<typename T> struct Vector2 {\n\
    T x;\n\
    const T& operator[](size_t i) const { return this->*_v[i]; }\n\
    T& operator[](size_t i) { return this->*_v[i]; }\n\
    T length() const { return sqrt(x); }\n\
  };\n\
}\n";
    let parsed = parse_ast("POPVector.h", "objc", src).expect("objc is compiled in");
    let names: Vec<&str> = parsed.calls.keys().map(|(_, n)| n.as_str()).collect();
    assert!(!names.contains(&""), "empty call name: {:?}", parsed.calls);
    assert!(
        !parsed.refs.contains_key(""),
        "empty ref: {:?}",
        parsed.refs
    );
    assert!(
        names.contains(&"sqrt"),
        "real calls are kept: {:?}",
        parsed.calls
    );
}
