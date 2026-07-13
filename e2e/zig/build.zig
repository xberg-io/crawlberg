const std = @import("std");
const builtin = @import("builtin");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});
    const test_step = b.step("test", "Run tests");
    const ffi_path = b.option([]const u8, "ffi_path", "Path to directory containing libcrawlberg_ffi") orelse "../../target/release";
    const ffi_include = b.option([]const u8, "ffi_include_path", "Path to directory containing FFI header") orelse "../../crates/crawlberg-ffi/include";
    const ffi_path_abs = b.pathFromRoot(ffi_path);

    const crawlberg_module = b.addModule("crawlberg", .{
        .root_source_file = b.path("../../packages/zig/src/crawlberg.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    crawlberg_module.addLibraryPath(.{ .cwd_relative = ffi_path });
    crawlberg_module.addIncludePath(.{ .cwd_relative = ffi_include });
    crawlberg_module.linkSystemLibrary("crawlberg_ffi", .{});
    crawlberg_module.addRPath(.{ .cwd_relative = ffi_path_abs });

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

    const auth_module = b.createModule(.{
        .root_source_file = b.path("src/auth_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    auth_module.addImport("crawlberg", crawlberg_module);
    const auth_tests = b.addTest(.{
        .name = "auth_test",
        .root_module = auth_module,
        .use_llvm = true,
    });
    auth_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const auth_run = b.addRunArtifact(auth_tests);
    auth_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        auth_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        auth_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            auth_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    test_step.dependOn(&auth_run.step);

    const browser_module = b.createModule(.{
        .root_source_file = b.path("src/browser_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    browser_module.addImport("crawlberg", crawlberg_module);
    const browser_tests = b.addTest(.{
        .name = "browser_test",
        .root_module = browser_module,
        .use_llvm = true,
    });
    browser_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const browser_run = b.addRunArtifact(browser_tests);
    browser_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        browser_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        browser_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            browser_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    browser_run.step.dependOn(&auth_run.step);
    test_step.dependOn(&browser_run.step);

    const cache_module = b.createModule(.{
        .root_source_file = b.path("src/cache_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    cache_module.addImport("crawlberg", crawlberg_module);
    const cache_tests = b.addTest(.{
        .name = "cache_test",
        .root_module = cache_module,
        .use_llvm = true,
    });
    cache_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const cache_run = b.addRunArtifact(cache_tests);
    cache_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        cache_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        cache_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            cache_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    cache_run.step.dependOn(&browser_run.step);
    test_step.dependOn(&cache_run.step);

    const concurrent_module = b.createModule(.{
        .root_source_file = b.path("src/concurrent_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    concurrent_module.addImport("crawlberg", crawlberg_module);
    const concurrent_tests = b.addTest(.{
        .name = "concurrent_test",
        .root_module = concurrent_module,
        .use_llvm = true,
    });
    concurrent_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const concurrent_run = b.addRunArtifact(concurrent_tests);
    concurrent_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        concurrent_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        concurrent_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            concurrent_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    concurrent_run.step.dependOn(&cache_run.step);
    test_step.dependOn(&concurrent_run.step);

    const content_module = b.createModule(.{
        .root_source_file = b.path("src/content_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    content_module.addImport("crawlberg", crawlberg_module);
    const content_tests = b.addTest(.{
        .name = "content_test",
        .root_module = content_module,
        .use_llvm = true,
    });
    content_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const content_run = b.addRunArtifact(content_tests);
    content_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        content_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        content_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            content_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    content_run.step.dependOn(&concurrent_run.step);
    test_step.dependOn(&content_run.step);

    const cookies_module = b.createModule(.{
        .root_source_file = b.path("src/cookies_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    cookies_module.addImport("crawlberg", crawlberg_module);
    const cookies_tests = b.addTest(.{
        .name = "cookies_test",
        .root_module = cookies_module,
        .use_llvm = true,
    });
    cookies_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const cookies_run = b.addRunArtifact(cookies_tests);
    cookies_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        cookies_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        cookies_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            cookies_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    cookies_run.step.dependOn(&content_run.step);
    test_step.dependOn(&cookies_run.step);

    const crawl_module = b.createModule(.{
        .root_source_file = b.path("src/crawl_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    crawl_module.addImport("crawlberg", crawlberg_module);
    const crawl_tests = b.addTest(.{
        .name = "crawl_test",
        .root_module = crawl_module,
        .use_llvm = true,
    });
    crawl_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const crawl_run = b.addRunArtifact(crawl_tests);
    crawl_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        crawl_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        crawl_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            crawl_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    crawl_run.step.dependOn(&cookies_run.step);
    test_step.dependOn(&crawl_run.step);

    const download_module = b.createModule(.{
        .root_source_file = b.path("src/download_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    download_module.addImport("crawlberg", crawlberg_module);
    const download_tests = b.addTest(.{
        .name = "download_test",
        .root_module = download_module,
        .use_llvm = true,
    });
    download_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const download_run = b.addRunArtifact(download_tests);
    download_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        download_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        download_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            download_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    download_run.step.dependOn(&crawl_run.step);
    test_step.dependOn(&download_run.step);

    const encoding_module = b.createModule(.{
        .root_source_file = b.path("src/encoding_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    encoding_module.addImport("crawlberg", crawlberg_module);
    const encoding_tests = b.addTest(.{
        .name = "encoding_test",
        .root_module = encoding_module,
        .use_llvm = true,
    });
    encoding_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const encoding_run = b.addRunArtifact(encoding_tests);
    encoding_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        encoding_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        encoding_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            encoding_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    encoding_run.step.dependOn(&download_run.step);
    test_step.dependOn(&encoding_run.step);

    const engine_module = b.createModule(.{
        .root_source_file = b.path("src/engine_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    engine_module.addImport("crawlberg", crawlberg_module);
    const engine_tests = b.addTest(.{
        .name = "engine_test",
        .root_module = engine_module,
        .use_llvm = true,
    });
    engine_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const engine_run = b.addRunArtifact(engine_tests);
    engine_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        engine_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        engine_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            engine_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    engine_run.step.dependOn(&encoding_run.step);
    test_step.dependOn(&engine_run.step);

    const error_module = b.createModule(.{
        .root_source_file = b.path("src/error_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    error_module.addImport("crawlberg", crawlberg_module);
    const error_tests = b.addTest(.{
        .name = "error_test",
        .root_module = error_module,
        .use_llvm = true,
    });
    error_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const error_run = b.addRunArtifact(error_tests);
    error_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        error_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        error_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            error_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    error_run.step.dependOn(&engine_run.step);
    test_step.dependOn(&error_run.step);

    const filter_module = b.createModule(.{
        .root_source_file = b.path("src/filter_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    filter_module.addImport("crawlberg", crawlberg_module);
    const filter_tests = b.addTest(.{
        .name = "filter_test",
        .root_module = filter_module,
        .use_llvm = true,
    });
    filter_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const filter_run = b.addRunArtifact(filter_tests);
    filter_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        filter_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        filter_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            filter_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    filter_run.step.dependOn(&error_run.step);
    test_step.dependOn(&filter_run.step);

    const interaction_module = b.createModule(.{
        .root_source_file = b.path("src/interaction_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    interaction_module.addImport("crawlberg", crawlberg_module);
    const interaction_tests = b.addTest(.{
        .name = "interaction_test",
        .root_module = interaction_module,
        .use_llvm = true,
    });
    interaction_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const interaction_run = b.addRunArtifact(interaction_tests);
    interaction_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        interaction_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        interaction_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            interaction_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    interaction_run.step.dependOn(&filter_run.step);
    test_step.dependOn(&interaction_run.step);

    const links_module = b.createModule(.{
        .root_source_file = b.path("src/links_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    links_module.addImport("crawlberg", crawlberg_module);
    const links_tests = b.addTest(.{
        .name = "links_test",
        .root_module = links_module,
        .use_llvm = true,
    });
    links_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const links_run = b.addRunArtifact(links_tests);
    links_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        links_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        links_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            links_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    links_run.step.dependOn(&interaction_run.step);
    test_step.dependOn(&links_run.step);

    const map_module = b.createModule(.{
        .root_source_file = b.path("src/map_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    map_module.addImport("crawlberg", crawlberg_module);
    const map_tests = b.addTest(.{
        .name = "map_test",
        .root_module = map_module,
        .use_llvm = true,
    });
    map_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const map_run = b.addRunArtifact(map_tests);
    map_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        map_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        map_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            map_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    map_run.step.dependOn(&links_run.step);
    test_step.dependOn(&map_run.step);

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
    markdown_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
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
    markdown_run.step.dependOn(&map_run.step);
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
    metadata_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
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

    const proxy_module = b.createModule(.{
        .root_source_file = b.path("src/proxy_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    proxy_module.addImport("crawlberg", crawlberg_module);
    const proxy_tests = b.addTest(.{
        .name = "proxy_test",
        .root_module = proxy_module,
        .use_llvm = true,
    });
    proxy_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const proxy_run = b.addRunArtifact(proxy_tests);
    proxy_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        proxy_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        proxy_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            proxy_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    proxy_run.step.dependOn(&metadata_run.step);
    test_step.dependOn(&proxy_run.step);

    const rate_limit_module = b.createModule(.{
        .root_source_file = b.path("src/rate_limit_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    rate_limit_module.addImport("crawlberg", crawlberg_module);
    const rate_limit_tests = b.addTest(.{
        .name = "rate_limit_test",
        .root_module = rate_limit_module,
        .use_llvm = true,
    });
    rate_limit_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const rate_limit_run = b.addRunArtifact(rate_limit_tests);
    rate_limit_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        rate_limit_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        rate_limit_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            rate_limit_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    rate_limit_run.step.dependOn(&proxy_run.step);
    test_step.dependOn(&rate_limit_run.step);

    const redirect_module = b.createModule(.{
        .root_source_file = b.path("src/redirect_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    redirect_module.addImport("crawlberg", crawlberg_module);
    const redirect_tests = b.addTest(.{
        .name = "redirect_test",
        .root_module = redirect_module,
        .use_llvm = true,
    });
    redirect_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const redirect_run = b.addRunArtifact(redirect_tests);
    redirect_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        redirect_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        redirect_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            redirect_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    redirect_run.step.dependOn(&rate_limit_run.step);
    test_step.dependOn(&redirect_run.step);

    const robots_module = b.createModule(.{
        .root_source_file = b.path("src/robots_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    robots_module.addImport("crawlberg", crawlberg_module);
    const robots_tests = b.addTest(.{
        .name = "robots_test",
        .root_module = robots_module,
        .use_llvm = true,
    });
    robots_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const robots_run = b.addRunArtifact(robots_tests);
    robots_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        robots_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        robots_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            robots_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    robots_run.step.dependOn(&redirect_run.step);
    test_step.dependOn(&robots_run.step);

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
    scrape_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
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
    scrape_run.step.dependOn(&robots_run.step);
    test_step.dependOn(&scrape_run.step);

    const sitemap_module = b.createModule(.{
        .root_source_file = b.path("src/sitemap_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    sitemap_module.addImport("crawlberg", crawlberg_module);
    const sitemap_tests = b.addTest(.{
        .name = "sitemap_test",
        .root_module = sitemap_module,
        .use_llvm = true,
    });
    sitemap_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const sitemap_run = b.addRunArtifact(sitemap_tests);
    sitemap_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        sitemap_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        sitemap_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            sitemap_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    sitemap_run.step.dependOn(&scrape_run.step);
    test_step.dependOn(&sitemap_run.step);

    const stealth_module = b.createModule(.{
        .root_source_file = b.path("src/stealth_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    stealth_module.addImport("crawlberg", crawlberg_module);
    const stealth_tests = b.addTest(.{
        .name = "stealth_test",
        .root_module = stealth_module,
        .use_llvm = true,
    });
    stealth_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const stealth_run = b.addRunArtifact(stealth_tests);
    stealth_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        stealth_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        stealth_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            stealth_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    stealth_run.step.dependOn(&sitemap_run.step);
    test_step.dependOn(&stealth_run.step);

    const strategy_module = b.createModule(.{
        .root_source_file = b.path("src/strategy_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    strategy_module.addImport("crawlberg", crawlberg_module);
    const strategy_tests = b.addTest(.{
        .name = "strategy_test",
        .root_module = strategy_module,
        .use_llvm = true,
    });
    strategy_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const strategy_run = b.addRunArtifact(strategy_tests);
    strategy_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        strategy_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        strategy_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            strategy_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    strategy_run.step.dependOn(&stealth_run.step);
    test_step.dependOn(&strategy_run.step);

    const validation_module = b.createModule(.{
        .root_source_file = b.path("src/validation_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    validation_module.addImport("crawlberg", crawlberg_module);
    const validation_tests = b.addTest(.{
        .name = "validation_test",
        .root_module = validation_module,
        .use_llvm = true,
    });
    validation_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const validation_run = b.addRunArtifact(validation_tests);
    validation_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        validation_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        validation_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            validation_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    validation_run.step.dependOn(&strategy_run.step);
    test_step.dependOn(&validation_run.step);

    const warc_module = b.createModule(.{
        .root_source_file = b.path("src/warc_test.zig"),
        .target = target,
        .optimize = optimize,
        .link_libc = true,
    });
    warc_module.addImport("crawlberg", crawlberg_module);
    const warc_tests = b.addTest(.{
        .name = "warc_test",
        .root_module = warc_module,
        .use_llvm = true,
    });
    warc_tests.root_module.addRPath(.{ .cwd_relative = ffi_path_abs });
    const warc_run = b.addRunArtifact(warc_tests);
    warc_run.setEnvironmentVariable("CRAWLBERG_ALLOW_PRIVATE_NETWORK", "true");
    if (mock_server_url) |_url| {
        warc_run.setEnvironmentVariable("MOCK_SERVER_URL", _url);
    }
    if (mock_servers_json) |_json| {
        warc_run.setEnvironmentVariable("MOCK_SERVERS", _json);
    }
    {
        var _it = mock_servers_map.iterator();
        while (_it.next()) |_entry| {
            warc_run.setEnvironmentVariable(_entry.key_ptr.*, _entry.value_ptr.*);
        }
    }
    warc_run.step.dependOn(&validation_run.step);
    test_step.dependOn(&warc_run.step);
}
