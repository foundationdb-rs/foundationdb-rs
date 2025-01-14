#![no_main]

use foundationdb_profiling::arbitrary::errors::ToBytesError;
use foundationdb_profiling::arbitrary::to_bytes::{ToBytes, ToBytesWithProtocolVersion};
use foundationdb_profiling::arbitrary::writer::Writer;
use foundationdb_profiling::events::ProfilingEvent;
use foundationdb_profiling::protocol_version::ProtocolVersion;
use libfuzzer_sys::arbitrary;
use libfuzzer_sys::arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use std::ops::Range;

pub fn choose_one<T>(items: &[T], choice: u8) -> &T {
    &items[choice as usize % items.len()]
}

pub fn correction_map(x: usize, in_map: Range<usize>, out_map: Range<usize>) -> usize {
    (x - in_map.start) * (out_map.end - out_map.start + 1) / (in_map.end - in_map.start + 1)
        + out_map.start
}

#[derive(Debug)]
struct BatchedEvents {
    protocol_version: ProtocolVersion,
    events: Vec<ProfilingEvent>,
}

impl Arbitrary<'_> for BatchedEvents {
    fn arbitrary(u: &mut Unstructured<'_>) -> arbitrary::Result<Self> {
        let protocol_version = ProtocolVersion::arbitrary(u)?;

        let x: u8 = u.arbitrary()?;
        let x = correction_map(
            x as usize,
            Range { start: 0, end: 255 },
            Range { start: 1, end: 5 },
        );

        let mut events = vec![];
        for _i in 0..x {
            let event = ProfilingEvent::arbitrary(u)?;
            events.push(event);
        }

        Ok(BatchedEvents {
            protocol_version,
            events,
        })
    }
}

impl ToBytes for BatchedEvents {
    fn to_bytes(&self, writer: &mut Writer<'_>) -> Result<(), ToBytesError> {
        let BatchedEvents {
            protocol_version,
            events,
        } = self;
        protocol_version.to_bytes(writer)?;
        events.to_bytes_with_protocol_version(writer, protocol_version)?;
        Ok(())
    }
}

fuzz_target!(|events: BatchedEvents| {
    let mut buffer = vec![];
    let mut writer = Writer::new(&mut buffer);
    events.to_bytes(&mut writer).unwrap();
    let size = writer.position();
    let data = writer.into_inner().into_inner().to_vec();
});
