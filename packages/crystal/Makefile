# crawlberg Crystal — build and run
#
# Usage:
#   make              Build FFI + Crystal example
#   make example      Build and run the example binary
#   make crystal      Build the Crystal library (also regenerates bindings)
#   make run          Build and run the example
#   make clean        Remove build artifacts

# Try to find alef in PATH or common development locations.
# REPO is the repo root (two dirs up from packages/crystal).
ALEF     ?= $(shell command -v alef 2>/dev/null || \
            test -f $(REPO)/../alef/target/debug/alef && echo "$(REPO)/../alef/target/debug/alef" || \
            test -f $(REPO)/../../dsisnero/alef/target/debug/alef && echo "$(REPO)/../../dsisnero/alef/target/debug/alef" || \
            echo "alef")
CARGO    ?= cargo
CRYSTAL  ?= crystal
REPO     ?= $(abspath $(dir $(lastword $(MAKEFILE_LIST)))../..)

FFI_LIB  := $(REPO)/target/release/libcrawlberg_ffi.a
EXAMPLE  := $(REPO)/packages/crystal/bin/example
FFI_DIR  := $(REPO)/target/release
PC_DIR   := $(REPO)/packages/crystal
PC_FILE  := $(PC_DIR)/crawlberg_ffi.pc
LIB_NAME := crawlberg_ffi

.PHONY: all ffi crystal example run clean

all: example

# Generate a pkg-config .pc file so the Crystal binding can find the FFI lib
# via @[Link(pkg-config: "crawlberg_ffi")]. The Makefile sets PKG_CONFIG_PATH
# so users don't need manual --link-flags.
$(PC_FILE): $(FFI_LIB)
	@mkdir -p $(PC_DIR)
	@echo "prefix=$(REPO)" > $@
	@echo "libdir=$(FFI_DIR)" >> $@
	@echo "includedir=$(REPO)/crates/crawlberg-ffi/include" >> $@
	@echo "" >> $@
	@echo "Name: $(LIB_NAME)" >> $@
	@echo "Description: C FFI bindings for Crawlberg" >> $@
	@echo "Version: 1.0.3" >> $@
	@echo "Libs: -L\$${libdir} -l$(LIB_NAME)" >> $@
	@echo "Cflags: -I\$${includedir}" >> $@

$(FFI_LIB):
	cd $(REPO) && $(ALEF) generate --lang ffi
	cd $(REPO) && $(CARGO) build -p crawlberg-ffi --release

ffi: $(FFI_LIB)

$(EXAMPLE): $(FFI_LIB) $(PC_FILE)
	cd $(REPO) && $(ALEF) generate --lang crystal
	cd $(REPO)/packages/crystal && \
	  $(CRYSTAL) build examples/example.cr -o bin/example \
	    --link-flags="-L$(FFI_DIR) -Wl,-rpath,$(FFI_DIR)"

crystal: $(EXAMPLE)

example: crystal
	$(CRYSTAL) run examples/example.cr --link-flags="-L$(FFI_DIR) -Wl,-rpath,$(FFI_DIR)"

run: example

clean:
	rm -f $(EXAMPLE) $(PC_FILE)
	cd $(REPO) && $(CARGO) clean -p crawlberg-ffi --release 2>/dev/null || true
