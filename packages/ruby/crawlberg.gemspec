# frozen_string_literal: true

Gem::Specification.new do |spec|
  spec.name = "crawlberg"
  spec.version = "1.3.1"
  spec.authors       = ["Xberg Team"]
  spec.summary       = "High-performance web crawling engine"
  spec.description   = "High-performance web crawling engine"
  spec.homepage      = "https://github.com/xberg-io/crawlberg"

  spec.license       = "MIT"

  spec.required_ruby_version = ">= 3.2.0"
  spec.metadata["keywords"] = %w[crawl scrape spider web].join(",")
  spec.metadata["rubygems_mfa_required"] = "true"

  candidate_files    = Dir.glob(%w[README* LICENSE* lib/**/* ext/**/* sig/**/* Steepfile]).select { |f| File.file?(f) }
  spec.files         = candidate_files.reject { |f| f.include?("/native/target/") || f.include?("/native/tmp/") }
  spec.require_paths = ["lib"]
  spec.extensions    = ["ext/crawlberg_rb/native/extconf.rb"]

  spec.add_dependency "rb_sys", ">= 0.9", "< 0.9.128"
  spec.add_dependency "sorbet-runtime", "~> 0.5"
end
