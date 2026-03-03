defmodule Gateway.Router do
  use Plug.Router

  plug :match
  plug Plug.Parsers, parsers: [:json], json_decoder: Jason
  plug :dispatch

  get "/health" do
    compute_host = System.get_env("COMPUTE_HOST", "compute")
    store_host = System.get_env("STORE_HOST", "store")

    compute_ok = health_check("http://#{compute_host}:8081/health")
    store_ok = health_check("http://#{store_host}:8082/health")

    if compute_ok and store_ok do
      send_resp(conn, 200, "ok")
    else
      send_resp(conn, 503, "unhealthy")
    end
  end

  post "/compute" do
    compute_host = System.get_env("COMPUTE_HOST", "compute")
    url = "http://#{compute_host}:8081/compute"

    case Req.post(url, json: conn.body_params) do
      {:ok, %{status: 200, body: body}} ->
        conn |> put_resp_content_type("application/json") |> send_resp(200, Jason.encode!(body))
      {:ok, %{status: status}} ->
        send_resp(conn, status, "error")
      {:error, _} ->
        send_resp(conn, 502, "Bad Gateway")
    end
  end

  get "/store/:key" do
    store_host = System.get_env("STORE_HOST", "store")
    url = "http://#{store_host}:8082/store/#{key}"

    case Req.get(url) do
      {:ok, %{status: 200, body: body}} ->
        conn |> put_resp_content_type("application/json") |> send_resp(200, Jason.encode!(body))
      {:ok, %{status: 404}} ->
        send_resp(conn, 404, "Not Found")
      _ ->
        send_resp(conn, 502, "Bad Gateway")
    end
  end

  put "/store/:key" do
    store_host = System.get_env("STORE_HOST", "store")
    url = "http://#{store_host}:8082/store/#{key}"

    case Req.put(url, json: conn.body_params) do
      {:ok, %{status: 200}} -> send_resp(conn, 200, "ok")
      _ -> send_resp(conn, 502, "Bad Gateway")
    end
  end

  match _ do
    send_resp(conn, 404, "Not Found")
  end

  defp health_check(url) do
    case Req.get(url) do
      {:ok, %{status: 200}} -> true
      _ -> false
    end
  end
end
