namespace A {
    namespace B {
        public class C {
            public int Value => Compute();
            int Compute() { return Other.Run(1); }
        }
    }
    class D { void M() { var c = new B.C(); c.Compute(); } }
}
