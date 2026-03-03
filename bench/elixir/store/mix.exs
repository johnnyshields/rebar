defmodule Store.MixProject do
  use Mix.Project

  def project do
    [app: :store, version: "0.1.0", elixir: "~> 1.17",
     start_permanent: Mix.env() == :prod, deps: deps()]
  end

  def application do
    [extra_applications: [:logger], mod: {Store.Application, []}]
  end

  defp deps do
    [{:plug_cowboy, "~> 2.7"}, {:jason, "~> 1.4"}]
  end
end
