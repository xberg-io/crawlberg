const std = @import("std");
const builtin = @import("builtin");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const test_step = b.step("test", "Run tests");

    // Fetch the published Zig package from the registry.
    const crawlberg_dep = b.dependency("crawlberg", .{
        .target = target,
        .optimize = optimize,
    });
    const crawlberg_module = crawlberg_dep.module("crawlberg");
    const crawlberg_lib_path = crawlberg_dep.path("lib");
    const crawlberg_include_path = crawlberg_dep.path("include");
    crawlberg_module.addLibraryPath(crawlberg_lib_path);
    crawlberg_module.addIncludePath(crawlberg_include_path);
    crawlberg_module.linkSystemLibrary("crawlberg_ffi", .{});

    const _alloc = b.allocator;
    var mock_server_url: ?[]const u8 = b.graph.environ_map.get("MOCK_SERVER_URL");
    var mock_servers_json: ?[]const u8 = null;
    var mock_servers_map = std.StringHashMap([]const u8).init(_alloc);
    if (mock_server_url == null) {
        const _bin = b.pathFromRoot("../rust/target/release/mock-server");
        const _fixtures = b.pathFromRoot("../../fixtures");
        var _threaded = std.Io.Threaded.init(_alloc, .{});
        const _io = _threaded.io();
        const _spawned = std.process.spawn(_io, .{
            .argv = &.{ _bin, _fixtures },
            .stdin = .pipe,
            .stdout = .pipe,
            .stderr = .inherit,
        });
        if (_spawned) |_child| {
            // The child is intentionally not awaited: it lives for the duration
            // of the `zig build` process, which spans test execution.
            const _stdout = _child.stdout.?;
            var _buf: [65536]u8 = undefined;
            var _file_reader = _stdout.readerStreaming(_io, &_buf);
            const _r = &_file_reader.interface;
            // The mock server needs a moment to bind its listeners before it
            // emits `MOCK_SERVER_URL=` and `MOCK_SERVERS=`. Under
            // `std.Io.Threaded` the pipe reader is non-blocking: a read can add
            // zero bytes (which `Reader.fillMore` reports as "no data yet", NOT
            // end-of-stream). Accumulate with `fillMore`, sleeping briefly when a
            // read makes no progress, until the complete `MOCK_SERVERS=` line is
            // buffered or the budget (~3s) is exhausted. (Do not use
            // `takeDelimiterExclusive` here: it treats a zero-byte read as a
            // terminal empty token and gives up before the lines are flushed.)
            var _i: usize = 0;
            while (_i < 600) : (_i += 1) {
                const _data = _r.buffered();
                if (std.mem.indexOf(u8, _data, "MOCK_SERVERS=")) |_pos| {
                    if (std.mem.indexOfScalar(u8, _data[_pos..], '\n') != null) break;
                }
                const _before = _r.bufferedLen();
                _r.fillMore() catch break;
                if (_r.bufferedLen() == _before) {
                    std.Io.sleep(_io, std.Io.Duration.fromMilliseconds(5), .awake) catch {};
                }
            }
            var _lines = std.mem.splitScalar(u8, _r.buffered(), '\n');
            while (_lines.next()) |_line_raw| {
                const _line = std.mem.trim(u8, _line_raw, " \r\t");
                if (std.mem.startsWith(u8, _line, "MOCK_SERVER_URL=")) {
                    mock_server_url = _alloc.dupe(u8, _line["MOCK_SERVER_URL=".len..]) catch null;
                } else if (std.mem.startsWith(u8, _line, "MOCK_SERVERS=")) {
                    const _json = _line["MOCK_SERVERS=".len..];
                    mock_servers_json = _alloc.dupe(u8, _json) catch null;
                    if (std.json.parseFromSlice(std.json.Value, _alloc, _json, .{})) |_parsed| {
                        if (_parsed.value == .object) {
                            var _entries = _parsed.value.object.iterator();
                            while (_entries.next()) |_entry| {
                                if (_entry.value_ptr.* == .string) {
                                    const _key = std.fmt.allocPrint(_alloc, "MOCK_SERVER_{s}", .{_entry.key_ptr.*}) catch continue;
                                    for (_key) |*_c| _c.* = std.ascii.toUpper(_c.*);
                                    const _val = _alloc.dupe(u8, _entry.value_ptr.*.string) catch continue;
                                    mock_servers_map.put(_key, _val) catch {};
                                }
                            }
                        }
                    } else |_| {}
                }
            }
        } else |_| {
            // Binary not built — leave mock_server_url null so tests surface a
            // clear connection error rather than a build failure.
        }
    }

    const markdown_module = b.createModule(.{
        .root_source_file = b.path("src/markdown_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    markdown_module.addImport("crawlberg", crawlberg_module);
    const markdown_tests = b.addTest(.{
        .name = "markdown_test",
        .root_module = markdown_module,
        .use_llvm = true,
    });
    const markdown_run = b.addRunArtifact(markdown_tests);
    markdown_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        markdown_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        markdown_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            markdown_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    test_step.dependOn(&markdown_run.step);

    const metadata_module = b.createModule(.{
        .root_source_file = b.path("src/metadata_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    metadata_module.addImport("crawlberg", crawlberg_module);
    const metadata_tests = b.addTest(.{
        .name = "metadata_test",
        .root_module = metadata_module,
        .use_llvm = true,
    });
    const metadata_run = b.addRunArtifact(metadata_tests);
    metadata_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        metadata_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        metadata_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            metadata_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    metadata_run.step.dependOn(&markdown_run.step);
    test_step.dependOn(&metadata_run.step);

    const scrape_module = b.createModule(.{
        .root_source_file = b.path("src/scrape_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    scrape_module.addImport("crawlberg", crawlberg_module);
    const scrape_tests = b.addTest(.{
        .name = "scrape_test",
        .root_module = scrape_module,
        .use_llvm = true,
    });
    const scrape_run = b.addRunArtifact(scrape_tests);
    scrape_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        scrape_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        scrape_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            scrape_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    scrape_run.step.dependOn(&metadata_run.step);
    test_step.dependOn(&scrape_run.step);
}
