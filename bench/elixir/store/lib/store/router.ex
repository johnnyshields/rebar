defmodule Store.Router do
  use Plug.Router

  plug :match
  plug Plug.Parsers, parsers: [:json], json_decoder: Jason
  plug :dispatch

  get "/health" do
    send_resp(conn, 200, "ok")
  end

  get "/store/:key" do
    case Store.KeyAgent.get_value(key) do
      {:ok, nil} -> send_resp(conn, 404, "Not Found")
      {:ok, value} ->
        conn |> put_resp_content_type("application/json")
             |> send_resp(200, Jason.encode!(%{key: key, value: value}))
      :not_found -> send_resp(conn, 404, "Not Found")
    end
  end

  put "/store/:key" do
    value = conn.body_params["value"]
    Store.KeyAgent.put_value(key, value)
    send_resp(conn, 200, "ok")
  end

  match _ do
    send_resp(conn, 404, "Not Found")
  end
end
