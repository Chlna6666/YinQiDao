# YinQiDao Host-managed WebAssembly lifecycle

## Developer contract

YinQiDao uses Rust **Wasmtime 48.0.1**, WASI Preview 2, and the Component Model.
Plugins only implement the `plugins/wit/yinqidao-plugin.wit` exports.
There is **no plugin GC API**, no requirement to manage compiled artifacts,
and no cache/instance eviction callback in the WIT contract.

- A new `Store<PluginStoreData>`, WASI context and resource table are created per
  invocation. On success, error or cancellation these are released by Rust
  ownership and RAII; no guest code is called to free Host resources.
- One process-wide Engine and immutable Host/WASI linker are shared.
  The compiled `Component` cache is shared between calls, is bounded using
  Host LRU policy, and is registered with the background GC controller.
- A compiled Component that has not been accessed for 10 minutes may be evicted
  during the Host GC sweep (30-second default interval). In-flight calls hold
  their own `Arc<Component>` and are not interrupted by eviction.
- On disable/update/uninstall, Host invalidates compiled components and any
  eligible pooled resources; a generation fence prevents an in-flight
  compilation from repopulating an obsolete cache entry.
- The disk cache and authenticated streaming cache have separate budget and
  lease-aware collection policies. Guest code cannot change either budget.

## Resource limits and execution

Defaults (Host policy, not plugin settings):

| Resource | Limit |
| --- | ---: |
| Linear memory per Store | 64 MiB |
| Component resource handles per Store | 4096 |
| Wasm tables per Store | 4 |
| Core/component instances per Store | 32 |
| Guest execution fuel per Store | 500,000,000 |
| Epoch interval | 10 ms |
| Guest execution deadline | 30 s |
| Compiled Component cache | 16 ready entries |
| Compiled Component idle TTL | 10 minutes |
| Background Host GC interval | 30 seconds |

The Host combines an outer wall-clock timeout with a Wasmtime
fuel/epoch budget. The real-time audio callback never invokes guest WASM,
network requests, cache cleanup or garbage collection.

## Caveats and resource ownership

WebAssembly guest heap allocations are governed by the guest's own Rust
allocator and are bounded by the Host linear-memory limit. The Host cannot
reliably reclaim *individual live objects inside a running guest* without
a compatible guest allocator/GC implementation. Instead, the Host reclaims
the entire Store/guest heap at the end of each invocation. This isolation
avoids cross-account guest globals, leaked handles and stateful pool reuse.

Some future operations could safely reuse Store/Instance pairs within a
verified account/route scope, but doing so requires resetting guest globals,
WASI tables, execution fuel and resource handles before reuse. The current
implementation intentionally does **not** recycle Store/Instance pairs.
The warm instance pool infrastructure exists, but is not active in production
calls; the compiled Component cache is the performance reuse boundary.

## Writing a plugin

1. Build an ABI-compatible `wasm32-wasip2` component.
2. Return metadata, lyrics, artwork and stream descriptors through WIT exports.
3. Use Host imports for HTTP and secrets; never open raw sockets or write to
   Host cache directories.
4. Return results or errors normally. Do not expose `gc()`, `free_all()`,
   `clear_cache()` or timer-driven cleanup APIs.
5. Use short-lived Rust values and avoid holding unbounded allocations in
   guest static state; Rust still drops local objects according to language
   semantics. Large downloaded audio should use the Host streaming path.

The Host owns plugin execution limits and cleanup. An account logout,
provider disable or package change should be handled by the Host revocation
path, not by a plugin's custom cleanup script.

## Performance verification

Benchmark separately:
- Warm/cold compile latency and `instantiate_async` latency.
- Guest-call p50/p95 duration and fuel use.
- Idle compiled-cache entry count, GC sweep latency and dropped entry count.
- Peak process RSS, per-Store linear-memory budget and live resource handles.
- Audio underruns, streaming rebuffer rate and UI frame time.

Do not attribute lower RSS or latency to a GC change without comparing these
measurements under an equivalent plugin call workload.
