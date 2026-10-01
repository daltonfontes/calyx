# Calyx runtime (C). Cargo builds everything into one static library
# (target/release/libcalyx_runtime.a): the C interpreter, the Rust I/O
# layer and the shared verifier. C programs link only that archive.

CC      ?= cc
CFLAGS  ?= -std=c11 -O2 -Wall -Wextra -Wpedantic -Werror
BUILD   := build
ROOT    := ..
LIB     := $(ROOT)/target/release/libcalyx_runtime.a
LDLIBS  := -lpthread -ldl -lm

.PHONY: all test clean lib

all: lib

lib:
	cargo build --release -p calyx-runtime --manifest-path $(ROOT)/Cargo.toml

$(LIB): lib

# The values module has no dependencies: test it on its own.
$(BUILD)/test_json: tests/test_json.c src/json.c src/json.h | $(BUILD)
	$(CC) $(CFLAGS) -Isrc tests/test_json.c src/json.c -lm -o $@

$(BUILD)/test_verify: tests/test_verify.c $(LIB) | $(BUILD)
	$(CC) $(CFLAGS) -Iinclude $< $(LIB) $(LDLIBS) -o $@

$(BUILD)/test_run: tests/test_run.c $(LIB) | $(BUILD)
	$(CC) $(CFLAGS) -Iinclude $< $(LIB) $(LDLIBS) -o $@

test: lib $(BUILD)/test_json $(BUILD)/test_verify $(BUILD)/test_run
	$(BUILD)/test_json
	$(BUILD)/test_verify
	$(BUILD)/test_run

$(BUILD):
	mkdir -p $@

clean:
	rm -rf $(BUILD)
