const a = () => 1, b = () => 2;

const curried = () => () => 1;

class Holder {
  handler = () => helper();
  static make = function named() { return new Holder(); };
  #secret() { return 1; }
}

namespace Outer {
  export function inside() { return 1; }
  export namespace Inner {
    export const deep = () => 2;
  }
}

enum Mixed {
  plain = 1,
  "with-dash" = 2,
  'quoted' = 3,
  Last,
}

export default class {
  run() { return go(); }
}

const api = {
  "get": () => fetch(),
  'post': function () { return send(); },
  nested: { put: () => 1 },
};

describe("suite", () => {
  it("works", function () {
    expect(1).toBe(1);
  });
});
