use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::Path,
    time::Duration,
};

use crate::{Av3aSampleEntry, probe_av3a_bytes};

const MAX_MOOV_BYTES: u64 = 64 * 1024 * 1024;
const MAX_SAMPLE_COUNT: usize = 10_000_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Av3aSampleTiming {
    pub sample_index: usize,
    pub start_tick: u64,
    pub duration_ticks: u32,
    pub timescale: u32,
}

impl Av3aSampleTiming {
    pub fn start_time(self) -> Duration {
        ticks_to_duration(self.start_tick, self.timescale)
    }

    pub fn duration(self) -> Duration {
        ticks_to_duration(u64::from(self.duration_ticks), self.timescale)
    }
}

#[derive(Clone, Copy, Debug)]
struct SampleLocation {
    offset: u64,
    size: u32,
    start_tick: u64,
    duration_ticks: u32,
}

#[derive(Debug)]
struct ParsedTrack {
    entry: Av3aSampleEntry,
    timescale: u32,
    duration_ticks: u64,
    samples: Vec<SampleLocation>,
}

/// Non-fragmented ISO-BMFF demuxer for an `av3a` Audio Vivid track.
///
/// GY/T 420-2025 defines each `av3a` track sample as exactly one `aatf_frame()`, so the sample
/// table can feed `Avs3Decoder` directly without an intermediate elementary-stream repacketizer.
/// The demuxer precomputes file offsets once from stsc + stco/co64 + stsz and keeps the packet Vec
/// caller-owned/reusable on the read path.
pub struct Av3aIsoBmffDemuxer {
    file: File,
    entry: Av3aSampleEntry,
    timescale: u32,
    duration_ticks: u64,
    samples: Vec<SampleLocation>,
    next_sample: usize,
}

impl Av3aIsoBmffDemuxer {
    pub fn open(path: &Path) -> io::Result<Option<Self>> {
        let mut file = File::open(path)?;
        let file_len = file.metadata()?.len();
        let Some(moov) = read_top_level_moov(&mut file, file_len)? else {
            return Ok(None);
        };
        let Some(track) = parse_av3a_track(&moov, file_len)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            file,
            entry: track.entry,
            timescale: track.timescale,
            duration_ticks: track.duration_ticks,
            samples: track.samples,
            next_sample: 0,
        }))
    }

    pub fn sample_entry(&self) -> &Av3aSampleEntry {
        &self.entry
    }

    pub fn sample_count(&self) -> usize {
        self.samples.len()
    }

    pub fn duration(&self) -> Duration {
        ticks_to_duration(self.duration_ticks, self.timescale)
    }

    pub fn position(&self) -> Duration {
        let tick = self
            .samples
            .get(self.next_sample)
            .map(|sample| sample.start_tick)
            .unwrap_or(self.duration_ticks);
        ticks_to_duration(tick, self.timescale)
    }

    /// Seek to the independently decodable sample containing `position`.
    ///
    /// Audio Vivid `av3a` samples are SAP type 1, so no preroll frame is required. Positions at or
    /// beyond the track duration select EOF.
    pub fn seek(&mut self, position: Duration) {
        let target = duration_to_ticks(position, self.timescale);
        if target >= self.duration_ticks {
            self.next_sample = self.samples.len();
            return;
        }

        let mut low = 0usize;
        let mut high = self.samples.len();
        while low < high {
            let middle = low + (high - low) / 2;
            if self.samples[middle].start_tick <= target {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        self.next_sample = low.saturating_sub(1);
    }

    /// Read one complete `aatf_frame()` sample into a reusable destination buffer.
    pub fn next_sample_into(
        &mut self,
        packet: &mut Vec<u8>,
    ) -> io::Result<Option<Av3aSampleTiming>> {
        let Some(sample) = self.samples.get(self.next_sample).copied() else {
            packet.clear();
            return Ok(None);
        };

        self.file.seek(SeekFrom::Start(sample.offset))?;
        packet.resize(sample.size as usize, 0);
        self.file.read_exact(packet)?;
        let timing = Av3aSampleTiming {
            sample_index: self.next_sample,
            start_tick: sample.start_tick,
            duration_ticks: sample.duration_ticks,
            timescale: self.timescale,
        };
        self.next_sample += 1;
        Ok(Some(timing))
    }
}

#[derive(Clone, Copy)]
struct MemoryBox<'a> {
    kind: [u8; 4],
    raw: &'a [u8],
    payload: &'a [u8],
}

fn memory_boxes(bytes: &[u8]) -> io::Result<Vec<MemoryBox<'_>>> {
    let mut boxes = Vec::new();
    let mut cursor = 0usize;
    while cursor < bytes.len() {
        if bytes.len() - cursor < 8 {
            return Err(invalid("truncated ISO-BMFF box header"));
        }
        let size32 = be_u32(&bytes[cursor..cursor + 4])?;
        let kind: [u8; 4] = bytes[cursor + 4..cursor + 8]
            .try_into()
            .map_err(|_| invalid("invalid ISO-BMFF box type"))?;
        let (size, header) = if size32 == 1 {
            if bytes.len() - cursor < 16 {
                return Err(invalid("truncated ISO-BMFF large-size box"));
            }
            (be_u64(&bytes[cursor + 8..cursor + 16])?, 16usize)
        } else if size32 == 0 {
            ((bytes.len() - cursor) as u64, 8usize)
        } else {
            (u64::from(size32), 8usize)
        };
        if size < header as u64 {
            return Err(invalid("ISO-BMFF box size is smaller than its header"));
        }
        let end_u64 = (cursor as u64)
            .checked_add(size)
            .ok_or_else(|| invalid("ISO-BMFF box size overflow"))?;
        let end = usize::try_from(end_u64)
            .map_err(|_| invalid("ISO-BMFF box exceeds addressable memory"))?;
        if end > bytes.len() {
            return Err(invalid("ISO-BMFF child box exceeds its parent"));
        }
        boxes.push(MemoryBox {
            kind,
            raw: &bytes[cursor..end],
            payload: &bytes[cursor + header..end],
        });
        cursor = end;
    }
    Ok(boxes)
}

fn child<'a>(bytes: &'a [u8], kind: &[u8; 4]) -> io::Result<Option<MemoryBox<'a>>> {
    Ok(memory_boxes(bytes)?.into_iter().find(|item| &item.kind == kind))
}

fn read_top_level_moov(file: &mut File, file_len: u64) -> io::Result<Option<Vec<u8>>> {
    let mut cursor = 0u64;
    while cursor < file_len {
        if file_len - cursor < 8 {
            return Err(invalid("truncated top-level ISO-BMFF box header"));
        }
        file.seek(SeekFrom::Start(cursor))?;
        let mut header = [0u8; 16];
        file.read_exact(&mut header[..8])?;
        let size32 = be_u32(&header[..4])?;
        let kind: [u8; 4] = header[4..8]
            .try_into()
            .map_err(|_| invalid("invalid top-level ISO-BMFF box type"))?;
        let (size, header_len) = if size32 == 1 {
            file.read_exact(&mut header[8..16])?;
            (be_u64(&header[8..16])?, 16u64)
        } else if size32 == 0 {
            (file_len - cursor, 8u64)
        } else {
            (u64::from(size32), 8u64)
        };
        if size < header_len {
            return Err(invalid("top-level ISO-BMFF box size is invalid"));
        }
        let end = cursor
            .checked_add(size)
            .ok_or_else(|| invalid("top-level ISO-BMFF box size overflow"))?;
        if end > file_len {
            return Err(invalid("top-level ISO-BMFF box exceeds file length"));
        }

        if &kind == b"moov" {
            let payload_len = size - header_len;
            if payload_len > MAX_MOOV_BYTES {
                return Err(unsupported("ISO-BMFF moov exceeds the demuxer safety limit"));
            }
            let mut payload = vec![0u8; payload_len as usize];
            file.seek(SeekFrom::Start(cursor + header_len))?;
            file.read_exact(&mut payload)?;
            return Ok(Some(payload));
        }
        cursor = end;
    }
    Ok(None)
}

fn parse_av3a_track(moov: &[u8], file_len: u64) -> io::Result<Option<ParsedTrack>> {
    for trak in memory_boxes(moov)? {
        if &trak.kind != b"trak" {
            continue;
        }
        if let Some(track) = parse_trak(trak.payload, file_len)? {
            return Ok(Some(track));
        }
    }
    Ok(None)
}

fn parse_trak(trak: &[u8], file_len: u64) -> io::Result<Option<ParsedTrack>> {
    let Some(mdia) = child(trak, b"mdia")? else {
        return Ok(None);
    };
    let Some(minf) = child(mdia.payload, b"minf")? else {
        return Ok(None);
    };
    let Some(stbl) = child(minf.payload, b"stbl")? else {
        return Ok(None);
    };
    let Some(stsd) = child(stbl.payload, b"stsd")? else {
        return Ok(None);
    };
    let Some((entry, description_index)) = parse_av3a_stsd(stsd.payload)? else {
        return Ok(None);
    };

    let mdhd = child(mdia.payload, b"mdhd")?
        .ok_or_else(|| invalid("av3a track is missing mdhd"))?;
    let timescale = parse_mdhd_timescale(mdhd.payload)?;
    let stsz = child(stbl.payload, b"stsz")?;
    if stsz.is_none() && child(stbl.payload, b"stz2")?.is_some() {
        return Err(unsupported("compact stz2 sample sizes are not implemented yet"));
    }
    let sample_sizes = parse_stsz(
        stsz.ok_or_else(|| unsupported("fragmented/stsz-less av3a track is not implemented yet"))?
            .payload,
    )?;
    let stsc = parse_stsc(
        child(stbl.payload, b"stsc")?
            .ok_or_else(|| unsupported("fragmented/stsc-less av3a track is not implemented yet"))?
            .payload,
    )?;
    let chunk_offsets = if let Some(stco) = child(stbl.payload, b"stco")? {
        parse_stco(stco.payload)?
    } else if let Some(co64) = child(stbl.payload, b"co64")? {
        parse_co64(co64.payload)?
    } else {
        return Err(unsupported(
            "fragmented av3a track without stco/co64 is not implemented yet",
        ));
    };
    let stts = parse_stts(
        child(stbl.payload, b"stts")?
            .ok_or_else(|| invalid("av3a track is missing stts"))?
            .payload,
    )?;

    let mut samples = build_sample_locations(
        &sample_sizes,
        &stsc,
        &chunk_offsets,
        description_index,
        file_len,
    )?;
    let duration_ticks = apply_sample_timing(&mut samples, &stts)?;
    Ok(Some(ParsedTrack {
        entry,
        timescale,
        duration_ticks,
        samples,
    }))
}

fn parse_av3a_stsd(payload: &[u8]) -> io::Result<Option<(Av3aSampleEntry, u32)>> {
    if payload.len() < 8 {
        return Err(invalid("truncated stsd box"));
    }
    let entry_count = be_u32(&payload[4..8])? as usize;
    let entries = memory_boxes(&payload[8..])?;
    if entries.len() < entry_count {
        return Err(invalid("stsd entry_count exceeds available sample entries"));
    }
    for (index, sample_entry) in entries.into_iter().take(entry_count).enumerate() {
        if &sample_entry.kind == b"av3a" {
            let entry = probe_av3a_bytes(sample_entry.raw)
                .ok_or_else(|| invalid("invalid av3a AudioSampleEntry"))?;
            return Ok(Some((entry, index as u32 + 1)));
        }
    }
    Ok(None)
}

fn parse_mdhd_timescale(payload: &[u8]) -> io::Result<u32> {
    if payload.len() < 4 {
        return Err(invalid("truncated mdhd full-box header"));
    }
    let version = payload[0];
    let offset = match version {
        0 => 12usize,
        1 => 20usize,
        _ => return Err(unsupported("unsupported mdhd version")),
    };
    let timescale = be_u32(
        payload
            .get(offset..offset + 4)
            .ok_or_else(|| invalid("truncated mdhd timescale"))?,
    )?;
    if timescale == 0 {
        return Err(invalid("mdhd timescale must be non-zero"));
    }
    Ok(timescale)
}

fn parse_stsz(payload: &[u8]) -> io::Result<Vec<u32>> {
    if payload.len() < 12 {
        return Err(invalid("truncated stsz box"));
    }
    let constant_size = be_u32(&payload[4..8])?;
    let sample_count = be_u32(&payload[8..12])? as usize;
    if sample_count > MAX_SAMPLE_COUNT {
        return Err(unsupported("av3a sample count exceeds the demuxer safety limit"));
    }
    if constant_size != 0 {
        return Ok(vec![constant_size; sample_count]);
    }
    let required = 12usize
        .checked_add(sample_count.saturating_mul(4))
        .ok_or_else(|| invalid("stsz size overflow"))?;
    if payload.len() < required {
        return Err(invalid("truncated stsz sample-size array"));
    }
    let mut sizes = Vec::with_capacity(sample_count);
    for chunk in payload[12..required].chunks_exact(4) {
        sizes.push(be_u32(chunk)?);
    }
    Ok(sizes)
}

#[derive(Clone, Copy, Debug)]
struct StscEntry {
    first_chunk: u32,
    samples_per_chunk: u32,
    sample_description_index: u32,
}

fn parse_stsc(payload: &[u8]) -> io::Result<Vec<StscEntry>> {
    if payload.len() < 8 {
        return Err(invalid("truncated stsc box"));
    }
    let count = be_u32(&payload[4..8])? as usize;
    let required = 8usize
        .checked_add(count.saturating_mul(12))
        .ok_or_else(|| invalid("stsc size overflow"))?;
    if payload.len() < required || count == 0 {
        return Err(invalid("invalid stsc entry array"));
    }
    let mut entries = Vec::with_capacity(count);
    for raw in payload[8..required].chunks_exact(12) {
        let entry = StscEntry {
            first_chunk: be_u32(&raw[0..4])?,
            samples_per_chunk: be_u32(&raw[4..8])?,
            sample_description_index: be_u32(&raw[8..12])?,
        };
        if entry.first_chunk == 0 || entry.samples_per_chunk == 0 || entry.sample_description_index == 0 {
            return Err(invalid("stsc entries must use non-zero one-based fields"));
        }
        if let Some(previous) = entries.last()
            && previous.first_chunk >= entry.first_chunk
        {
            return Err(invalid("stsc first_chunk values must be strictly increasing"));
        }
        entries.push(entry);
    }
    if entries[0].first_chunk != 1 {
        return Err(invalid("stsc must begin at chunk 1"));
    }
    Ok(entries)
}

fn parse_stco(payload: &[u8]) -> io::Result<Vec<u64>> {
    parse_chunk_offsets(payload, 4)
}

fn parse_co64(payload: &[u8]) -> io::Result<Vec<u64>> {
    parse_chunk_offsets(payload, 8)
}

fn parse_chunk_offsets(payload: &[u8], width: usize) -> io::Result<Vec<u64>> {
    if payload.len() < 8 {
        return Err(invalid("truncated chunk-offset box"));
    }
    let count = be_u32(&payload[4..8])? as usize;
    let required = 8usize
        .checked_add(count.saturating_mul(width))
        .ok_or_else(|| invalid("chunk-offset array size overflow"))?;
    if payload.len() < required {
        return Err(invalid("truncated chunk-offset array"));
    }
    let mut offsets = Vec::with_capacity(count);
    for raw in payload[8..required].chunks_exact(width) {
        offsets.push(if width == 4 {
            u64::from(be_u32(raw)?)
        } else {
            be_u64(raw)?
        });
    }
    Ok(offsets)
}

#[derive(Clone, Copy, Debug)]
struct SttsEntry {
    sample_count: u32,
    sample_delta: u32,
}

fn parse_stts(payload: &[u8]) -> io::Result<Vec<SttsEntry>> {
    if payload.len() < 8 {
        return Err(invalid("truncated stts box"));
    }
    let count = be_u32(&payload[4..8])? as usize;
    let required = 8usize
        .checked_add(count.saturating_mul(8))
        .ok_or_else(|| invalid("stts size overflow"))?;
    if payload.len() < required || count == 0 {
        return Err(invalid("invalid stts entry array"));
    }
    let mut entries = Vec::with_capacity(count);
    for raw in payload[8..required].chunks_exact(8) {
        let entry = SttsEntry {
            sample_count: be_u32(&raw[0..4])?,
            sample_delta: be_u32(&raw[4..8])?,
        };
        if entry.sample_count == 0 || entry.sample_delta == 0 {
            return Err(invalid("stts entries must have non-zero count and delta"));
        }
        entries.push(entry);
    }
    Ok(entries)
}

fn build_sample_locations(
    sample_sizes: &[u32],
    stsc: &[StscEntry],
    chunk_offsets: &[u64],
    av3a_description_index: u32,
    file_len: u64,
) -> io::Result<Vec<SampleLocation>> {
    if sample_sizes.is_empty() {
        return Ok(Vec::new());
    }
    if chunk_offsets.is_empty() {
        return Err(invalid("non-empty av3a track has no chunks"));
    }

    let mut locations = Vec::with_capacity(sample_sizes.len());
    let mut sample_index = 0usize;
    let mut stsc_index = 0usize;
    for (chunk_zero, &chunk_offset) in chunk_offsets.iter().enumerate() {
        let chunk_number = chunk_zero as u32 + 1;
        while stsc_index + 1 < stsc.len()
            && chunk_number >= stsc[stsc_index + 1].first_chunk
        {
            stsc_index += 1;
        }
        let mapping = stsc[stsc_index];
        if mapping.sample_description_index != av3a_description_index {
            return Err(unsupported(
                "sample-description switching inside an av3a track is not implemented yet",
            ));
        }

        let mut offset = chunk_offset;
        for _ in 0..mapping.samples_per_chunk {
            let Some(&size) = sample_sizes.get(sample_index) else {
                return Err(invalid("stsc describes more samples than stsz"));
            };
            if size == 0 {
                return Err(invalid("av3a sample size must be non-zero"));
            }
            let end = offset
                .checked_add(u64::from(size))
                .ok_or_else(|| invalid("av3a sample file offset overflow"))?;
            if end > file_len {
                return Err(invalid("av3a sample exceeds file length"));
            }
            locations.push(SampleLocation {
                offset,
                size,
                start_tick: 0,
                duration_ticks: 0,
            });
            offset = end;
            sample_index += 1;
        }
    }
    if sample_index != sample_sizes.len() {
        return Err(invalid("stsc/stco do not cover all stsz samples"));
    }
    Ok(locations)
}

fn apply_sample_timing(samples: &mut [SampleLocation], stts: &[SttsEntry]) -> io::Result<u64> {
    let mut sample_index = 0usize;
    let mut tick = 0u64;
    for entry in stts {
        for _ in 0..entry.sample_count {
            let Some(sample) = samples.get_mut(sample_index) else {
                return Err(invalid("stts describes more samples than stsz"));
            };
            sample.start_tick = tick;
            sample.duration_ticks = entry.sample_delta;
            tick = tick
                .checked_add(u64::from(entry.sample_delta))
                .ok_or_else(|| invalid("av3a track duration overflow"))?;
            sample_index += 1;
        }
    }
    if sample_index != samples.len() {
        return Err(invalid("stts does not cover all av3a samples"));
    }
    Ok(tick)
}

fn duration_to_ticks(duration: Duration, timescale: u32) -> u64 {
    duration
        .as_secs()
        .saturating_mul(u64::from(timescale))
        .saturating_add(
            u64::from(duration.subsec_nanos())
                .saturating_mul(u64::from(timescale))
                / 1_000_000_000,
        )
}

fn ticks_to_duration(ticks: u64, timescale: u32) -> Duration {
    if timescale == 0 {
        return Duration::ZERO;
    }
    let scale = u64::from(timescale);
    let seconds = ticks / scale;
    let remainder = ticks % scale;
    let nanos = remainder.saturating_mul(1_000_000_000) / scale;
    Duration::new(seconds, nanos as u32)
}

fn be_u32(bytes: &[u8]) -> io::Result<u32> {
    Ok(u32::from_be_bytes(
        bytes
            .get(..4)
            .ok_or_else(|| invalid("truncated big-endian u32"))?
            .try_into()
            .map_err(|_| invalid("invalid big-endian u32"))?,
    ))
}

fn be_u64(bytes: &[u8]) -> io::Result<u64> {
    Ok(u64::from_be_bytes(
        bytes
            .get(..8)
            .ok_or_else(|| invalid("truncated big-endian u64"))?
            .try_into()
            .map_err(|_| invalid("invalid big-endian u64"))?,
    ))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn unsupported(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_location_builder_maps_variable_chunks_without_payload_copy() {
        let sizes = [10, 11, 12, 13, 14];
        let stsc = [
            StscEntry {
                first_chunk: 1,
                samples_per_chunk: 2,
                sample_description_index: 1,
            },
            StscEntry {
                first_chunk: 3,
                samples_per_chunk: 1,
                sample_description_index: 1,
            },
        ];
        let locations = build_sample_locations(&sizes, &stsc, &[100, 200, 300], 1, 1_000)
            .expect("locations");
        assert_eq!(locations.len(), 5);
        assert_eq!((locations[0].offset, locations[1].offset), (100, 110));
        assert_eq!((locations[2].offset, locations[3].offset), (200, 212));
        assert_eq!(locations[4].offset, 300);
    }

    #[test]
    fn stts_expansion_produces_monotonic_sample_times() {
        let mut samples = vec![
            SampleLocation { offset: 0, size: 1, start_tick: 0, duration_ticks: 0 };
            4
        ];
        let end = apply_sample_timing(
            &mut samples,
            &[
                SttsEntry { sample_count: 2, sample_delta: 1_024 },
                SttsEntry { sample_count: 2, sample_delta: 960 },
            ],
        )
        .expect("timing");
        assert_eq!(samples[0].start_tick, 0);
        assert_eq!(samples[1].start_tick, 1_024);
        assert_eq!(samples[2].start_tick, 2_048);
        assert_eq!(samples[3].start_tick, 3_008);
        assert_eq!(end, 3_968);
    }

    #[test]
    fn duration_tick_conversion_is_integer_and_stable() {
        let duration = ticks_to_duration(1_024, 48_000);
        assert_eq!(duration, Duration::new(0, 21_333_333));
        assert_eq!(duration_to_ticks(duration, 48_000), 1_023);
    }
}
