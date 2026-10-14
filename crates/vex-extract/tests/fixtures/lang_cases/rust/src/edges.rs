use {self};
use ::{self, a};
use std::{collections::*, fmt};

fn two_inner() {
    fn second() {}
    if true {
        fn first_in_block() {}
    }
    fn third() {}
}

fn shadow_typed(x: Foo) {
    let c = |x| x.go();
    x.go2();
}

fn shadow_expr() {
    let y = make_token();
    let c = |y| y.go();
    y.go3();
}

fn rebound() {
    let inner = make_token();
    let Wrapper(inner) = wrap();
    inner.go4();
}
