defmodule Store.Application do
  use Application

  @impl true
  def start(_type, _args) do
    children = [
      {Registry, keys: :unique, name: Store.KeyRegistry},
      {DynamicSupervisor, name: Store.KeySupervisor, strategy: :one_for_one},
      {Plug.Cowboy, scheme: :http, plug: Store.Router, options: [port: 8082]}
    ]
    opts = [strategy: :one_for_one, name: Store.Supervisor]
    Supervisor.start_link(children, opts)
  end
end
