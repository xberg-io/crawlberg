require "spec"

# Environment variables set before loading the binding
ENV["CRAWLBERG_ALLOW_PRIVATE_NETWORK"] ||= "true"

# Spawn the e2e mock server if MOCK_SERVER_URL is not already set.
if ENV["MOCK_SERVER_URL"]?.nil?
  mock_server_path = File.join(__DIR__, "..", "..", "rust", "target", "release", "mock-server")
  fixtures_path = File.join(__DIR__, "..", "..", "..", "fixtures")
  if File.exists?(mock_server_path)
    reader, writer = IO.pipe
    pid = Process.new(mock_server_path, [fixtures_path], output: writer)
    writer.close
    line = reader.gets
    if line && line.starts_with?("MOCK_SERVER_URL=")
      ENV["MOCK_SERVER_URL"] = line.lchop("MOCK_SERVER_URL=").strip
    end
    at_exit { Process.signal(Signal::TERM, pid.pid); pid.wait }
  else
    STDERR.puts "mock-server binary not found at #{mock_server_path}"
    STDERR.puts "Run: cargo build --release --manifest-path e2e/rust/Cargo.toml --bin mock-server"
    exit(1)
  end
end

require "crawlberg"
