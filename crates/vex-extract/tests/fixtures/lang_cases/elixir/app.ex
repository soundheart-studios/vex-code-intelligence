defmodule MyApp.Server do
  alias MyApp.{Repo, Cache}
  import Ecto.Query
  require Logger
  use GenServer

  def start(opts) do
    Repo.insert(opts)
    Logger.info("started")
    helper(opts)
  end

  def start(a, b) when is_integer(a), do: Cache.put(a, b)

  defp helper(x) do
    if x, do: inspect(x), else: Enum.map(x, &(&1))
  end

  defmacro trace(expr), do: quote(do: unquote(expr))
end

defmodule Other do
  def run, do: MyApp.Server.start([])
end
