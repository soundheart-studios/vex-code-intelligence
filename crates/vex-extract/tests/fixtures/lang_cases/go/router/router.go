package router

import (
	"bytes"
	fmtx "fmt"
	_ "embed"
	. "strings"
	raw `net/http`
	"github.com/acme/tree/v2"
)

type Nodes []*Node
type Grid [4]Cell
type Set[T comparable] struct {
	items map[T]struct{}
	list  []T
}

type Node struct {
	child, sibling *Node
	kids           []*Node
	names          []string
	buf            bytes.Buffer
	tree           *tree.Tree
	cells          [8]Cell
}

type Walker interface {
	Walk(n *Node) error
	fmtx.Stringer
	Reset()
}

type Mux struct {
	tree  *Node
	opts  Options
	ñodes Nodes
}

func (mx *Mux) Handle(p string) {
	mx.route(p)
	mx.tree.find(p)
	mx.opts.apply()
}

func (mx Mux) ByValue() { mx.route("v") }

func (s *Set[T]) Add(v T) { s.grow() }

func (*Mux) Unnamed() { helper() }

func (_ *Mux) Blank(other *Mux) { other.route("b") }

func (mx *Mux) Reassigned() {
	mx = NewMux()
	mx.route("r")
}

func (mx *Mux) Shadowed() {
	f := func(mx *Other) { mx.route("s") }
	f(nil)
	mx.route("outer")
}

func (mx *Mux) Bumped(n int) {
	for mx = range pool {
	}
	mx.route("x")
}

func Typed(w *Writer, cfg Config, opts ...Option) (int, error) {
	var b Buffer
	w.Write(nil)
	b.Reset()
	cfg.opts.apply()
	opts.each()
	return 0, nil
}

func Disagree(flag bool) {
	if flag {
		var v Alpha
		v.run()
	} else {
		var v Beta
		v.run()
	}
}

func Switches(x interface{}) {
	switch t := x.(type) {
	case *Node:
		t.walk()
	}
}

func Exprs(nodes Nodes, m *Mux) (n int, err error) {
	r := NewRouter()
	r.Handle()
	a, e := Open()
	a.Close()
	e.Error()
	for _, nd := range nodes {
		nd.walk()
	}
	first := nodes[0]
	first.walk()
	t := Tree{}
	t.Insert()
	p := &Tree{}
	p.Insert()
	root := m.tree
	root.walk()
	alias := root
	alias.walk()
	cmd := exec.Command("ls")
	cmd.Run()
	ñ := NewÑ()
	ñ.go()
	mine := m.NewChild()
	mine.walk()
	return 0, nil
}

func Results() (a, b int)          { return 0, 0 }
func Single() *Node                { return nil }
func Slice() []*Node               { return nil }
func Mixed() (n *Node, err error)  { return nil, nil }
func (m *Mux) Method() (Nodes, error) { return nil, nil }
func (s *Set[T]) Generic() T          { var z T; return z }
func NoResults()                   {}

func NonASCII() {
	x := NewÑ()
	x.walk()
	z := Café{}
	z.brew()
}

type Holder struct {
	ptrs *[]Node
}

func Commented() (a int, /* note */ err error) { return 0, nil }

func (p (*Mux)) Paren() int { return 0 }

func Boundaries(d Dynamo, i Impala, m mutator, s Self) {
	d.spin()
	i.run()
	m.flip()
	s.me()
}

func Variadic(items ...Item) {
	f := func(items Item) { items.use() }
	f(nil)
}

func SwitchShadow(x interface{}) {
	var t Node
	switch t := x.(type) {
	case *Node:
		t.walk()
	}
	t.walk()
}

func MultiVar() {
	var p, q = NewP(), NewQ()
	p.go2()
	q.go3()
}

func RangeKey(nodes Nodes) {
	for i, nd := range nodes {
		i.walk()
		nd.walk()
	}
}

func Deep() {
	a1 := NewX()
	a2 := a1
	a3 := a2
	a4 := a3
	a5 := a4
	a6 := a5
	a7 := a6
	a8 := a7
	a9 := a8
	a9.go4()
	a5.go5()
}

type impl struct{}
type dyn struct{}

func Words(a impl, b dyn) {
	a.run()
	b.run()
}
