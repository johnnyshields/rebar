defmodule Compute.Application do
  use Application

  @impl true
  def start(_type, _args) do
    children = [
      {Task.Supervisor, name: Compute.WorkerSupervisor},
      {Plug.Cowboy, scheme: :http, plug: Compute.Router, options: [port: 8081]}
    ]
    opts = [strategy: :one_for_one, name: Compute.Supervisor]
    Supervisor.start_link(children, opts)
  end
end
