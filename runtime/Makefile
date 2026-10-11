# Calyx runtime (C). Cargo builds everything into one static library
# (target/release/libcalyx_runtime.a): the C interpreter, the Rust I/O
# layer and the shared verifier. C programs link only that archive.

CC      ?= cc
CFLAGS  ?= -std=c11 -O2 -Wall -Wextra -Wpedantic -Werror
BUILD   := build
ROOT    := ..
LIB     := $(ROOT)/target/release/libcalyx_runtime.a
LDLIBS  := -lpthread -ldl -lm

.PHONY: all test clean lib fuzz fuzz-run

all: lib

lib:
	cargo build --release -p calyx-runtime --manifest-path $(ROOT)/Cargo.toml

$(LIB): lib

# Values and hashes have no dependencies: test them on their own.
$(BUILD)/test_json: tests/test_json.c src/json.c src/json.h src/sha256.c src/sha256.h | $(BUILD)
	$(CC) $(CFLAGS) -Isrc tests/test_json.c src/json.c src/sha256.c -lm -o $@

$(BUILD)/test_verify: tests/test_verify.c $(LIB) | $(BUILD)
	$(CC) $(CFLAGS) -Iinclude $< $(LIB) $(LDLIBS) -o $@

$(BUILD)/test_run: tests/test_run.c $(LIB) | $(BUILD)
	$(CC) $(CFLAGS) -Iinclude $< $(LIB) $(LDLIBS) -o $@

test: lib $(BUILD)/test_json $(BUILD)/test_verify $(BUILD)/test_run
	$(BUILD)/test_json
	$(BUILD)/test_verify
	$(BUILD)/test_run

# Fuzzing (clang with libFuzzer, AddressSanitizer and UBSan): the C code
# that reads what comes from outside, linked alone with fuzz/stubs.c in
# place of the Rust layer. `make fuzz` builds; `make fuzz-run` runs each
# target for FUZZ_SECONDS (default 60) on its corpus in fuzz/corpus/.
FUZZ_CC    ?= clang
FUZZ_FLAGS := -std=c11 -g -O1 -DCALYX_FUZZ -fsanitize=fuzzer,address,undefined \
              -fno-sanitize-recover=undefined -Isrc -Iinclude -Ifuzz
FUZZ_SRC   := src/exec.c src/json.c src/journal.c src/sha256.c fuzz/stubs.c
FUZZ_SECONDS ?= 60
FUZZERS    := $(BUILD)/fuzz_json $(BUILD)/fuzz_journal $(BUILD)/fuzz_answers $(BUILD)/fuzz_ir

$(BUILD)/fuzz_%: fuzz/fuzz_%.c $(FUZZ_SRC) src/*.h fuzz/stubs.h | $(BUILD)
	$(FUZZ_CC) $(FUZZ_FLAGS) $< $(FUZZ_SRC) -lpthread -lm -o $@

fuzz: $(FUZZERS)

# The IR fuzz_answers runs is compiled from fuzz/answers.clyx here, so it
# follows the IR format (fuzz/answers.ir.json is a copy, for running a
# target by hand without building the compiler). Each target runs in a directory of its own (entities write files where
# they run); what crashes is kept in $(BUILD)/crash-<target>-*.
fuzz-run: fuzz
	@cargo build --release -q -p calyx-cli --manifest-path $(ROOT)/Cargo.toml
	@$(ROOT)/target/release/calyx check fuzz/answers.clyx --ir-json > $(BUILD)/answers.ir.json
	@for f in json journal answers ir; do \
	  mkdir -p $(BUILD)/fuzz-new/$$f $(BUILD)/fuzz-work; \
	  echo "fuzz_$$f: $(FUZZ_SECONDS) s"; \
	  (cd $(BUILD)/fuzz-work && CALYX_FUZZ_IR=$(CURDIR)/$(BUILD)/answers.ir.json \
	    $(CURDIR)/$(BUILD)/fuzz_$$f -max_total_time=$(FUZZ_SECONDS) \
	    -max_len=16384 -timeout=10 -rss_limit_mb=4096 -print_final_stats=1 \
	    -artifact_prefix=$(CURDIR)/$(BUILD)/crash-$$f- \
	    $(CURDIR)/$(BUILD)/fuzz-new/$$f $(CURDIR)/fuzz/corpus/$$f) || exit 1; \
	done

$(BUILD):
	mkdir -p $@

clean:
	rm -rf $(BUILD)
