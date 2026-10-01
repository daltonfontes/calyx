/*
 * calyx_io.h - effects, implemented by the Rust I/O layer (runtime/rs/io.rs).
 *
 * Requests and answers are JSON texts. Answers are owned by the I/O layer
 * and released with calyx_string_free() (calyx_verify.h).
 *
 *   model: {"model", "prompt", "schema", "max_output", "timeout_ms"}
 *       -> {"ok": {"text", "input_tokens", "output_tokens", "ms"}}
 *   tool:  {"tool", "args", "max_output", "timeout_ms"}
 *       -> {"ok": {"text", "json", "truncated", "ms"}}
 *   both:  -> {"error": {"kind", "message"}}
 *
 * Temporary error kinds: Timeout, RateLimit, Unavailable, Network.
 * Others: Auth, BadRequest, Decode, ToolError, Config.
 */
#ifndef CALYX_IO_H
#define CALYX_IO_H

#ifdef __cplusplus
extern "C" {
#endif

char *calyx_io_model_call(const char *request_json);
char *calyx_io_tool_call(const char *request_json);

#ifdef __cplusplus
}
#endif

#endif /* CALYX_IO_H */
