# Integrating cache-reuse-utils into a host engine

This guide shows how an inference engine (for example DwarfStar) uses the C
ABI to plan and execute suffix-cache reuse. The library only computes: it
takes token IDs, pad positions and ranges, and returns plans, checks and
schedules. The engine keeps everything stateful: KV memory, archives, files,
threads, generations and the model itself. No call retains a pointer after it
returns.

## 1. Build and link

```sh
cargo build --release --locked -p cache-reuse-utils-ffi --target-dir /abs/target
cc -std=c11 -Iffi/include engine.c /abs/target/release/libcache_reuse_utils_ffi.a \
  -lpthread -ldl -lm
```

Include `ffi/include/cache_reuse_utils.h`. Every function returns
`CACHE_REUSE_COMPLETE` (0), `CACHE_REUSE_ERR_INVALID` (10) with a message in
the optional `error_out` buffer, or `CACHE_REUSE_ERR_PANIC` (14). A
`cache_reuse_verify_*` call that runs but finds a mismatch still returns
`COMPLETE` and reports the result through its `bool` output. Outputs are zeroed
before validation. Tokens are `int32_t`; pad positions and lengths are
`uint32_t`. If you link the static library from a Rust workspace, set
`[profile.release] panic = "unwind"` in that workspace's root manifest.

## 2. Keep a coordinate map per session

A padded KV cache holds a **physical** sequence: the logical prompt with pad
tokens inserted. Store, next to the session checkpoint, its physical length
and the strictly increasing pad positions. Keep the map up to date as the
cache changes:

| Engine event | Call |
|---|---|
| Plan materialized | `cache_reuse_materialize` writes the prepared tokens and pad positions |
| Tokens generated or appended | `cache_reuse_append_unpadded` returns the new physical length |
| Cache truncated or rolled back | `cache_reuse_truncate_pad_positions` returns how many pads to keep |
| Archive sealed for an interval | `cache_reuse_slice_pad_positions` returns the interval's relative pads |
| Before publishing a checkpoint | `cache_reuse_verify_matches_logical` checks physical minus pads equals logical |

Coordinate conversion is `cache_reuse_project_logical` and
`cache_reuse_logical_boundary`.

## 3. Per request: decide what to reuse

1. **Continuation.** `cache_reuse_common_prefix(request, checkpoint, pads)`.
   If the whole live logical sequence is a prefix of the request, append the
   rest. No archive work is needed.
2. **Pool choice (optional).** For the hot archive and each pool entry, fill a
   `cache_reuse_candidate` with its tokens, its pads and
   `planned_reusable_tokens`. That count is the result of a short-budget
   `cache_reuse_plan` against the entry, or 0 to score by common prefix
   alone. Then call `cache_reuse_select_candidate`. Its answer is
   `CACHE_REUSE_ACTION_KEEP_HOT`, `_RESTORE_POOL` (with `pool_index`) or
   `_FRESH`. The engine performs any restore.
3. **Plan.** `cache_reuse_plan(request, source, config, &result, ...)` returns
   reused ranges with source, original-target and prepared (physical) starts,
   plus any pad insertions. Then `cache_reuse_materialize` builds the
   physical target and its pad positions.
4. **Authorize pads.** When pads are inserted only next to separators (for
   example whitespace or newline token IDs), call
   `cache_reuse_verify_separator_map`. It checks the materialized map against
   the request, the carried prefix pads and the pad token.
   `cache_reuse_collect_separator_boundaries` lists the allowed insertion
   points, which can feed `config.allowed_insertions`.

## 4. Execute against an archive

1. **Clip.** `cache_reuse_clip_ranges_to_archive(ranges, archive_start,
   archive_end, target_cap, alignment, ...)` keeps only the part of each
   range that the archive holds and that lies below the aligned destination
   cap. Drop survivors shorter than your minimum, such as 128 tokens.
2. **Check pads per range.** `cache_reuse_verify_span_correspondence(source
   pads, target pads, range)` confirms that each reused span carries its pads
   to the same offsets.
3. **Schedule.** `cache_reuse_compile_archive_schedule(physical_len, ranges,
   &geometry, ops, capacity, &len, ...)`. If `ops` is too small, the call
   fails and sets `len` to the capacity needed, so you can retry. Set
   `cache_reuse_archive_geometry` from the engine's limits:
   `raw_arena_tokens` (rows between arena flushes), `max_import_chunk`,
   `encoder_acquire_threshold` (0 disables), `encoder_release_tail` and
   `head_alignment`.
4. **Run the ops in order.** The library has already verified the schedule,
   so the engine only dispatches:

   | `kind` | Engine action |
   |---|---|
   | `CACHE_REUSE_OP_ACQUIRE_ENCODER` / `_RELEASE_ENCODER` | Take or drop encoder residency |
   | `CACHE_REUSE_OP_PREFETCH_WINDOW` | Start reading source `[source_start, source_end)` in the background |
   | `CACHE_REUSE_OP_STAGE_SPAN` | Materialize `count` source rows from `source_start` for destination `position` |
   | `CACHE_REUSE_OP_IMPORT_CHUNK` | Copy `count` staged rows into the arena at `position`; flush if `flush_arena` |
   | `CACHE_REUSE_OP_PREFILL_CHUNK` | Compute `count` rows from `position` (captured before the covered head); flush if `flush_arena`; `defer_decoder` permits deferring decoder work into the next chunk |
   | `CACHE_REUSE_OP_COVERED_HEAD` | Compute logits at `position` and seal the new archive there |
   | `CACHE_REUSE_OP_FINAL_HEAD` | Compute logits at `position` (the physical length) |

   Before each op, check the engine-side preconditions the library cannot
   see: token equality, generation pins and cancellation.
5. **Reconcile.** After a full or partial execution, call
   `cache_reuse_reconcile_executed_ranges(planned, executed, physical_len,
   alignment, &summary, ...)`. It checks that the engine only shortened or
   skipped planned ranges, never moved them. It returns reused, gap, suffix
   and fresh token counts for receipts and metrics.

## 5. Replacing DwarfStar's built-in helpers

| DwarfStar helper | cache-reuse-utils call |
|---|---|
| `ds41_coordinates_slice` / `_append` / `_truncate` | `cache_reuse_slice_pad_positions` / `cache_reuse_append_unpadded` / `cache_reuse_truncate_pad_positions` |
| `ds41_coordinates_match` (with projection) | `cache_reuse_verify_matches_logical` |
| `ds41_multi_pad_correspondence` | `cache_reuse_verify_span_correspondence` |
| `ds41_boundary_map_authorized`, `ds41_even_carry_prefix` | `cache_reuse_verify_separator_map` |
| `ds41_pool_score`, `ds4_session_scr_pool_select` (decision only) | `cache_reuse_plan` per entry, then `cache_reuse_select_candidate` |
| `ds41_rebase_clip` | `cache_reuse_clip_ranges_to_archive` with `target_cap = (n & ~7) - 2` |
| `ds41_even_executed_subset` | `cache_reuse_reconcile_executed_ranges` |
| The import, gap and suffix loops in `ds41_scr_multi_selected` | `cache_reuse_compile_archive_schedule` plus an op dispatcher |

The engine keeps ownership and identity checks (`identity`, generations,
memory charging), the model calls, and file and GPU I/O.

## 6. Performance

`cargo run --release --example bench_planning -j 2` measures these helpers on
32K to 262K-token workloads. At 262,144 tokens on Apple silicon, every helper
takes well under 1 ms. For example, `verify_matches_logical` takes about
23 µs, `select_candidate` over 16 candidates about 0.22 ms, and
`compile_archive_schedule` with 32 ranges about 5 µs. They can therefore run
on the request path.
