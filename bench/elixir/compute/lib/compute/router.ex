defmodule Compute.Router do
  use Plug.Router

  plug :match
  plug Plug.Parsers, parsers: [:json], json_decoder: Jason
  plug :dispatch

  get "/health" do
    send_resp(conn, 200, "ok")
  end

  post "/compute" do
    n = conn.body_params["n"]

    if n < 0 or n > 92 do
      Task.Supervisor.async_nolink(Compute.WorkerSupervisor, fn ->
        raise "crash injection: invalid n"
      end)
      send_resp(conn, 500, "Internal Server Error")
    else
      task = Task.Supervisor.async_nolink(Compute.WorkerSupervisor, fn -> fib(n) end)
      case Task.yield(task, 5000) || Task.shutdown(task) do
        {:ok, result} ->
          conn |> put_resp_content_type("application/json")
               |> send_resp(200, Jason.encode!(%{result: result}))
        _ ->
          send_resp(conn, 500, "Timeout")
      end
    end
  end

  match _ do
    send_resp(conn, 404, "Not Found")
  end

  defp fib(n) when n <= 1, do: n
  defp fib(n) do
    Enum.reduce(2..n, {0, 1}, fn _, {a, b} -> {b, a + b} end) |> elem(1)
  end
end
