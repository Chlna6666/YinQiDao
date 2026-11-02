# YinQiDao Streaming Playback Host Protocol v1

This document defines **the cross-provider playback contract** implemented by YinQiDao Host.
The provider interface remains `StreamDescriptor` / `StreamRequest` in
`src/plugin/abi.rs` and `plugins/wit/`. The Host owns transport, buffering,
decoder, playback, cache and authorization. Do **not** implement HTTP audio
downloads independently in Netease, QQ Music or future providers.

## Contract and compatibility

```text
Provider::stream(StreamRequest) -> StreamDescriptor {url, headers, codec,
  bitrate, sample_rate, channels, expires_at_ms}

Host::prepare_remote_track_for_route
   -> StreamCache::materialize_progressively
   -> PluginPlaybackSource {private Host path and lease, is_progressive}
   -> DecoderStream / Symphonia MediaSource
   -> AudioWorker -> CPAL ring buffer
```

The legacy `materialize` entry point remains available for non-streamable
containers and for callers requiring a completely verified file. ABI v1
providers require **no source change**. URLs, signed headers, cookies and
cache paths are never exposed to the decoder or to other providers.

## Source state machine

| State | Meaning | Permitted action |
|---|---|---|
| Resolving | Guest request/route/account validation | Wait or cancel |
| Fetching | HTTP Range response being validated | Write bounded chunks |
| Ready | Continuous downloaded prefix reached startup threshold | Start decode |
| Buffering | Decoder reads past currently published prefix | Retry, never treat as EOF |
| Complete | All bytes written and validated | EOF is now final; publish cache |
| Failed | Host policy, network or validation failure | Propagate error, no successful EOF |
| Cancelled | Playback session invalidated | Discard pending decoder use |

The cache writer calls `ProgressiveSession::written(available)` **only after**
`write_all` succeeds, and `finish(None)` only after download validation.
The reader never sees uninitialized holes. Unexpected early EOF and
network failure must not trigger an automatic next song.

## Transport authority / isolation

1. Every new HTTP Range request passes the existing live route health,
   installed manifest, permission, domain, proxy, redirect and pinned-DNS
   security checks.
2. Cache identity includes plugin ID, provider ID, account ID, source ID,
   quality, codec properties and installed package revision. Signed URLs
   and authorization headers must not be used as cache identity.
3. A `206` response must have a valid, consistent `Content-Range` with
   identical total length between requests; strong ETag / Last-Modified
   validators are enforced by `If-Range`.
4. A `200` response is accepted only for the initial request with the
   existing full-body size limits. Subsequent offsets cannot silently
   restart the whole response.
5. Background speculative work shares the same bounded network
   concurrency and can be cancelled or displaced by foreground playback.
6. Only complete validated entries are promoted into the persistent cache.
   An active decoder retains the original live path while cache commit uses
   a same-volume hard link and atomic bucket commit.
7. If a filesystem does not support safe linking, playback may finish
   without persistent promotion. Do not move a file currently opened by
   a decoder on Windows.
8. A session retains its cache space reservation until the last playback
   reader is released; GC cannot remove an active partial download.

## Decoder and buffer semantics

- `Read::read` returning `WouldBlock` means *more data may arrive*.
  `Read::read` returning `Ok(0)` means **verified complete EOF only**.
  `BrokenPipe` denotes terminal download failure.
- The audio worker retains command processing while buffering, including
  Pause, Stop, Next, Previous, track replacement and Seek.
- No HTTP, filesystem wait, Wasmtime guest call, decoder or mutex is added
  to the CPAL audio output callback.
- The current v1 implementation supports contiguous sequential progressive
  streams for MP3, FLAC, Ogg/Vorbis, Opus, WAV and elementary AAC as
  advertised by the descriptor. AVS3, M4A/MP4 and unknown containers
  retain their completed-file fallback until their probing/indexing and
  seeking are separately validated.
- Seeking outside downloaded contiguous bytes can temporarily buffer until
  sequential download reaches the destination. **Prioritized sparse Range
  seeking is not implemented in v1** and must not be claimed.
- A network response with mismatched ranges, changed resource validator or
  revoked permission fails closed. End-of-file is never fabricated to hide it.

## Public provider interface

Providers return only a `StreamDescriptor` through their existing
`stream(StreamRequest)` export. New providers must never rely on
Netease/QQ-specific codec names or private Host cache internals.

```rust
// Existing ABI v1 contract: NO new required exports.
pub struct StreamRequest {
    pub track: SourceTrackRef,
    pub quality: Option<String>,
}
pub struct StreamDescriptor {
    pub url: String,
    pub headers: Vec<KeyValue>,
    pub codec: Option<String>,
    pub bitrate: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub expires_at_ms: Option<u64>,
}
```

`PluginPlaybackSource::is_progressive()` is a Host-side capability probe,
not a plugin ABI promise. The Host may choose the legacy complete-download
path based on codec, server behavior, cache hit or policy.

## Test / measurement requirements

- **Correctness:** early `WouldBlock` vs real EOF; network failure vs EOF;
  decoder resume without resetting position; rapid cancellation; incomplete
  cache never accepted as complete; account/provider isolation.
- **Integration:** HTTP 206/200/416, malformed Content-Range, truncated
  body, weak ETag, redirect to disallowed host, revoked grant mid-download,
  expired descriptor, file cleanup on Windows.
- **Playback:** next/previous during buffering; seek inside cached prefix
  and outside prefix; offline hit; format probe and bitrate changes.
- **Performance:** measure click-to-first-audible (TTFA) p50/p95,
  buffer-underruns/hour, rebuffer ratio, network bytes, cache hit rate,
  decoder CPU time and CPAL underruns separately.

## Forward protocol work

The next protocol revision may add byte-range availability maps, prioritized
sparse fetching on seek, continuous codec/container indexing, bounded retry
policies and adaptive prebuffer duration. It must retain the security and
verified-completion invariants above.
