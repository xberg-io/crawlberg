# crawlberg

High-performance web crawling engine

## Installation

Install Zig from [ziglang.org](https://ziglang.org/download/).

## Building

```sh
zig build
zig build test
```

## Usage

Add to your `build.zig.zon`:

```text
.dependencies = .{
    .crawlberg = .{
        .path = "path/to/crawlberg",
    },
},
```

## License

MIT
