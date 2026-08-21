const crawlberg = @import("crawlberg");

// `generate_citations` isn't a zero-arg, primitive-returning function this seed can safely call
// generically — its per-parameter allocator/ownership/JSON conversion contract is not
// knowable here — so this *references* it rather than calling it. That is not a weaker
// `@hasDecl`: taking the address forces Zig to semantically analyse the wrapper's body
// and to resolve the extern C symbol that body calls, neither of which a comptime
// `@hasDecl` does. Measured on Zig 0.16.0 with a deleted extern symbol: `@hasDecl` exits
// 0 ("All 1 tests passed"); this line exits 1 with `undefined symbol: ... referenced
// by ...`, matching a real call (the positive control). Same split for a type error in
// the wrapper body with no extern involved.
//
// LIMIT — read this before trusting a green run. This proves the symbol EXISTS and the
// wrapper typechecks. It does NOT prove the symbol is CORRECT. A C-level ABI change that
// preserves the symbol name is invisible to it: the linker resolves by name and C
// symbols carry no type information, so if the C signature changes and the generated Zig
// `extern` declaration is regenerated to match it, both move together and nothing ever
// disagrees. This closes "the symbol does not exist". It leaves "the symbol lies" wide
// open. Create-only scaffold seed. ~keep
test "crawlberg.generate_citations symbol resolves" {
    _ = &crawlberg.generate_citations;
}
