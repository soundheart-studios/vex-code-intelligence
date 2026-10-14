import Ky, { Options as KyOptions, retry } from "./ky";
import * as util from 'util';

function* gen() {
  const k = new Ky();
  k.go();
}

function shadowArrow() {
  const k = new Ky();
  const one = k => k.go();
  const two = (k) => k.run();
  k.done();
}

function nestedDecl() {
  function inner() {
    const k = 5;
  }
  const k = new Ky();
  k.go();
}

function bumped() {
  let k = new Ky();
  k++;
  k.go();
}

function augmented() {
  let k = new Ky();
  k ||= other();
  k.go();
}

function reassigned() {
  let k = new Ky();
  k = other();
  k.go();
}

function viaVar() {
  var k = new Ky();
  k.go();
}

function capturedWrite() {
  const k = new Ky();
  const f = () => { k = 3; };
  k.go();
}
