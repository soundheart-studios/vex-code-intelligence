// Widget: every C++ ownership and call-marker shape.
#include <string>
#include <vector>

namespace outer {
namespace inner {

class LEVELDB_EXPORT Widget : public Base {
 public:
  Widget();
  ~Widget();
  int run(const std::string& s LIFETIME_BOUND) const;
  bool operator<=>(const Widget& o) const { return compare(o); }
  int opera::tor()(int x) { return call(x); }
  friend void swap(Widget& a, Widget& b) { a.swap_impl(b); }
  template <typename T> T get() const { return T(); }
 private:
  Block block_;
  std::vector<int> items_;
  Iterator* iter_;
  int raw_[4];
  void (*callback_)(int);
  struct { int depth; } anon_;
};

Widget::Widget() : block_() { init(); }
Widget::~Widget() { cleanup(); }

int Widget::run(const std::string& s) const {
  iter_->Next();
  this->block_.Reset();
  block_.Size();
  items_.size();
  Block local;
  local.Size();
  Block* ptr = nullptr;
  ptr->Reset();
  auto [a, b] = pair();
  a.first();
  for (const Item& item : list()) { item.check(); }
  auto fn = [this, &s](int v) { return v; };
  fn(1);
  Status::OK();
  std::max(1, 2);
  Holder<int>::make();
  ::global_fn();
  outer::helper();
  unknown_->Poke();
  struct LocalReporter { void Report() { report(); } };
  LocalReporter r;
  r.Report();
  return 0;
}

}  // namespace inner

void inner::Widget::Later() { later(); }
void ::outer::Absolute() { absolute(); }

namespace {
struct Hidden { Config cfg; int count; };
int Anon() { Hidden h; return h.cfg.level(); }
}

}  // namespace outer

namespace a::b {
union Value { int i; float f; };
enum class Mode : int { kFast, kSlow };
}

void outer::inner::Widget::Deep() { deep(); }

PURE_FUNCTION constexpr std::basic_string_view<char> substr(int pos) { return view(pos); }
BASE_EXPORT std::vector<std::string> SplitString(const std::string& in) { return split(in); }
void Guarded() ENABLE_IF_ATTR(x, "msg") { guarded(); }
PURE_FUNCTION static int Leading() { return lead(); }

template <typename T>
class Box {
 public:
  T value() const { return value_; }
 private:
  T value_;
};

template <>
int Box<int>::valueconstexpr () const { return 0; }
