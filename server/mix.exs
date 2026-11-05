defmodule AetherHub.MixProject do
  use Mix.Project

  def project do
    [
      app: :aether_hub,
      version: "0.1.0",
      elixir: "~> 1.20",
      elixirc_paths: elixirc_paths(Mix.env()),
      start_permanent: Mix.env() == :prod,
      deps: deps(),
      releases: [aether_hub: [include_executables_for: [:unix]]]
    ]
  end

  def application do
    [
      extra_applications: [:logger, :crypto],
      mod: {AetherHub.Application, []}
    ]
  end

  defp elixirc_paths(:test), do: ["lib", "test/support"]
  defp elixirc_paths(_), do: ["lib"]

  defp deps do
    [
      {:bandit, "~> 1.13"},
      {:plug, "~> 1.20"},
      {:ecto_sqlite3, "~> 0.25.0"}
    ]
  end
end
