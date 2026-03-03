defmodule Gateway.MixProject do
  use Mix.Project

  def project do
    [app: :gateway, version: "0.1.0", elixir: "~> 1.17",
     start_permanent: Mix.env() == :prod, deps: deps()]
  end

  def application do
    [extra_applications: [:logger], mod: {Gateway.Application, []}]
  end

  defp deps do
    [{:plug_cowboy, "~> 2.7"}, {:jason, "~> 1.4"}, {:req, "~> 0.5"}]
  end
end
