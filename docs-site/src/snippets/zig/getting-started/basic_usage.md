```zig title="Zig"
const std = @import("std");
const crawlberg = @import("crawlberg");

pub fn main() !void {
    var gpa: std.heap.DebugAllocator(.{}) = .init;
    defer _ = gpa.deinit();
    const allocator = gpa.allocator();

    // Simplest case: scrape a single page with default settings.
    const scrape_json = try crawlberg.scrape(null, "https://example.com/");
    defer std.heap.c_allocator.free(scrape_json);
    var scrape_parsed = try std.json.parseFromSlice(std.json.Value, allocator, scrape_json, .{});
    defer scrape_parsed.deinit();
    const result = &scrape_parsed.value;
    const title = result.object.get("metadata").?.object.get("title").?;
    std.debug.print("Title: {s}\n", .{if (title == .string) title.string else ""});
    std.debug.print("Status: {d}\n", .{result.object.get("status_code").?.integer});
    std.debug.print("Links found: {d}\n", .{result.object.get("links").?.array.items.len});

    // Crawl from a seed URL, limited to one hop and a handful of pages.
    const crawl_json = try crawlberg.crawl(
        "{\"max_depth\":1,\"max_pages\":5}",
        "https://en.wikipedia.org/wiki/Web_scraping",
    );
    defer std.heap.c_allocator.free(crawl_json);
    var crawl_parsed = try std.json.parseFromSlice(std.json.Value, allocator, crawl_json, .{});
    defer crawl_parsed.deinit();
    const crawl_result = &crawl_parsed.value;
    std.debug.print("Pages crawled: {d}\n", .{crawl_result.object.get("pages").?.array.items.len});
}
```
