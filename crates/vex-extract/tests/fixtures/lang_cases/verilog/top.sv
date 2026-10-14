package pkg;
  function automatic int add(int a, int b);
    return a + b;
  endfunction
endpackage

module top(input clk);
  import pkg::*;
  import other::thing;
  counter u0(.clk(clk));
  counter u1(.clk(clk));
  task automatic run();
    $display("x");
    helper();
  endtask
  function int helper();
    return pkg::add(1, 2);
  endfunction
endmodule

module counter(input clk);
endmodule

class Packet;
  function void show();
    $display("p");
  endfunction
endclass
