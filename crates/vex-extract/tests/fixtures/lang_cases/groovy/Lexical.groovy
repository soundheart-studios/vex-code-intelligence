class Lexical {
    String name = defaultName
    def outer() {
        def local() { inner() }
        build(1)
        assert(ready)
        throw(problem)
        in(x)
        as(y)
        def(z)
        try(w)
        else(v)
    }
    Configure(x) { setup() }
    Lexical(int n) { this.n = n }
}
class Alias = Other
def later() { call() }
{ stray() }
