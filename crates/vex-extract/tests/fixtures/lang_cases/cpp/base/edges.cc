// leading macro right after a comment at the top of the file
PURE_FUNCTION inline int TopLeading() { return top(); }

class COMPONENT_EXPORT("str") Quoted {};

void TakeParams(Fn (cb), const T& s LIFETIME_BOUND);
void TakeDefault(const T& s LIFETIME_BOUND = T());
void TakeArray(int a LIFETIME_BOUND[4]);
void TakeCapture(const T& s LIFETIME_CAPTURE_BY_THIS);

int AfterAssign() { int z = MY_MACRO inline; return z; }

PURE_FUNCTION constexpr std::basic_string_view<char> operator+(int x) { return view(x); }
PURE_FUNCTION std::basic_string_view<char> substr2(int pos) { return view2(pos); }

namespace ns1 { class Dup { int x_; void a() { x_.go(); } }; }
namespace ns2 { class Dup { Block x_; void b() { x_.go(); } }; }

class Holder {
  ::std::string name_;
  void Use() {
    Block blk;
    auto f = [blk]() { blk.Size(); };
    std::pair<int, int> p;
    p.first.go();
    Config c;
    c.inner.apply();
  }
};

namespace outer {
void outer::SameScope() { same(); }
namespace inner {
void ::other::Abs() { abs_call(); }
}
}

void Broken(int a, ) { struct Loc { void m() { loc(); } }; }

namespace ns3 { class Dup2 { Block y_; void c() { y_.go(); } }; }
namespace ns4 { class Dup2 { int y_; void d() { y_.go(); } }; }
