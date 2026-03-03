defmodule Store.KeyAgent do
  use Agent

  def start_link(key) do
    Agent.start_link(fn -> nil end, name: via(key))
  end

  def get_value(key) do
    case Registry.lookup(Store.KeyRegistry, key) do
      [{pid, _}] -> {:ok, Agent.get(pid, & &1)}
      [] -> :not_found
    end
  end

  def put_value(key, value) do
    pid = get_or_start(key)
    Agent.update(pid, fn _ -> value end)
  end

  defp get_or_start(key) do
    case Registry.lookup(Store.KeyRegistry, key) do
      [{pid, _}] -> pid
      [] ->
        case DynamicSupervisor.start_child(Store.KeySupervisor, {__MODULE__, key}) do
          {:ok, pid} -> pid
          {:error, {:already_started, pid}} -> pid
        end
    end
  end

  defp via(key), do: {:via, Registry, {Store.KeyRegistry, key}}
end
