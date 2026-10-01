# Calyx runtime (C). Links the Rust verifier as a static library.

CC      ?= cc
CFLAGS  ?= -std=c11 -O2 -Wall -Wextra -Wpedantic -Werror
BUILD   := build
ROOT    := ..
VERIFY  := $(ROOT)/target/release/libcalyx_check.a
LDLIBS  := -lpthread -ldl -lm

.PHONY: all test clean verifier

all: $(BUILD)/libcalyx_runtime.a

verifier:
	cargo build --release -p calyx-check --manifest-path $(ROOT)/Cargo.toml

$(VERIFY): verifier

$(BUILD)/runtime.o: src/runtime.c include/calyx_runtime.h include/calyx_verify.h | $(BUILD)
	$(CC) $(CFLAGS) -Iinclude -c $< -o $@

$(BUILD)/libcalyx_runtime.a: $(BUILD)/runtime.o
	ar rcs $@ $^

$(BUILD)/test_verify: tests/test_verify.c $(BUILD)/libcalyx_runtime.a $(VERIFY)
	$(CC) $(CFLAGS) -Iinclude $< $(BUILD)/libcalyx_runtime.a $(VERIFY) $(LDLIBS) -o $@

test: verifier $(BUILD)/test_verify
	$(BUILD)/test_verify

$(BUILD):
	mkdir -p $@

clean:
	rm -rf $(BUILD)
