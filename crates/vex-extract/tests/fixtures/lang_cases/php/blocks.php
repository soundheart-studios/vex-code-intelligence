<?php
namespace A\B {
    class Inner { function go() { return 1; } }
}
namespace {
    function globalFn() { return \A\B\Inner::class; }
}
