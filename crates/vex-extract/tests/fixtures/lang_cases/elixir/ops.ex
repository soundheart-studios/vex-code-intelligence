defmodule Ops do
  def a <> b, do: a
  def plain(x), do: x
  def one(0), do: 0; def one(n), do: n
end
