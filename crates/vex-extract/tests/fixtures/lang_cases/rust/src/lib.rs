use std::fmt;
use std::collections::{self, HashMap, btree_map::Entry as BEntry};
use crate::error::{Error, nested::{Deep, self}};
use serde::*;
use tokio as rt;
use plain;

pub struct Parser<'a> {
    input: &'a str,
    buf: &'a mut Vec<u8>,
    boxed: Box<dyn Reader>,
    items: Vec<Item>,
    raw: [u8; 4],
    cfg: Config,
    me: Option<Self>,
}

pub struct Pair(u32, String);

mod inner {
    pub struct Hidden { pub depth: Depth }
    pub fn make() -> Result<(Hidden, Depth), Error> { todo!() }
}

mod declared;

extern "C" {
    fn ffi_call(x: i32) -> i32;
}

impl<'a> Parser<'a> {
    pub fn new(input: &'a str) -> Self { Parser::build(input) }

    fn run(&mut self, cfg: &Config, r: &'a mut Reader, d: &dyn Driver, i: impl Iter) -> Option<Token> {
        self.cfg.apply();
        self.next();
        cfg.opts.apply();
        r.read();
        d.drive();
        i.step();
        let w: Writer = Writer::new();
        w.flush();
        let s: Self = todo!();
        s.run2();
        let tok = self.lex()?;
        tok.kind();
        let made = make_token()?;
        made.kind();
        let plain = make_token();
        plain.kind();
        let (a, b) = split();
        a.left();
        b.right();
        for item in self.items.iter() {
            item.check();
        }
        if let Some(found) = lookup() {
            found.use_it();
        }
        match fetch() {
            Ok(v) => v.consume(),
            Err(e) => e.report(),
        }
        let f = |x| x.call_me();
        let g = |y: Thing| y.call_typed();
        let field = self.cfg;
        field.apply2();
        let elem = self.items[0];
        elem.check2();
        let lit = Config { level: 1 };
        lit.apply3();
        println!("{}", 1);
        vec![1, 2];
        crate::macros::trace!("x");
        f(1);
        g(Thing);
        None
    }

    fn nested(&self) -> u32 {
        if true {
            helper_one()
        } else {
            fn helper_one() -> u32 { 2 }
            helper_one()
        };
        let c = || { fn in_closure() {} in_closure() };
        c();
        0
    }
}

impl fmt::Display for Parser<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result { write!(f, "p") }
}

impl Default for Pair {
    fn default() -> Self { Pair(0, String::new()) }
}

trait Reader {
    fn read(&mut self) -> Option<u8>;
    fn peek(&self) -> u8 { 0 }
}

enum Token { Word(String), Num(u32), End }

fn make_token() -> Result<Token, Error> { Ok(Token::End) }
fn split() -> (Left, Right) { todo!() }
fn lookup() -> Option<Found> { None }
fn fetch() -> Result<Value, Error> { todo!() }
fn tuple_ret() -> (u8, String, Vec<u8>) { todo!() }
fn opt_self() -> Option<Self> { None }
fn returns_impl() -> impl Iterator<Item = u8> { 0..1 }
fn returns_mut<'a>(x: &'a mut Item) -> &'a mut Item { x }
fn mutable(mutable_thing: mutator) { mutable_thing.go(); }
