defmodule Crawlberg.MixProject do
  use Mix.Project

  def project do
    [
      app: :crawlberg,
      version: "1.1.0",
      elixir: "~> 1.14",
      elixirc_paths: ["lib", Path.expand("../../packages/elixir/native/crawlberg_nif/src", __DIR__)],
      rustler_crates: [
        crawlberg_nif: [
          mode: :release,
          targets: [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64-pc-windows-msvc"
          ]
        ]
      ],
      description: "High-performance web crawling engine",
      package: package(),
      deps: deps()
    ]
  end

  defp package do
    [
      licenses: ["MIT"],
      links: %{"GitHub" => "https://github.com/xberg-io/crawlberg"},
      files:
        ~w(lib .formatter.exs mix.exs README* checksum-*.exs native/crawlberg_nif/Cargo.toml native/crawlberg_nif/Cargo.lock native/crawlberg_nif/src)
    ]
  end

  defp deps do
    [
      {:jason, "~> 1.4"},
      {:rustler, "~> 0.38", runtime: false},
      {:rustler_precompiled, "~> 0.9"},
      {:credo, "~> 1.7", only: [:dev, :test], runtime: false},
      {:ex_doc, "~> 0.40", only: :dev, runtime: false}
    ]
  end
end
