use std::collections::BTreeMap;

pub mod fc;
pub mod gc;

pub use fc::Fc;
pub use gc::Gc;

pub(crate) struct MavlinkBuffer {
  pub sequence: u64,
  pub buffer: BTreeMap<u64, Vec<u8>>
}

impl MavlinkBuffer {
  pub fn new() -> Self {
    MavlinkBuffer {
      sequence: 0,
      buffer: BTreeMap::new()
    }
  }

  pub fn len(&self) -> usize {
    self.buffer.len()
  }

  /// Returns the last seqnum and constructs payload [seqnum, data]
  pub fn last(&self) -> Option<(u64, Vec<u8>)> {
    self.buffer.last_key_value().map(|(seqnum, data)|
      (*seqnum, [&seqnum.to_be_bytes()[..], &data[..]].concat().to_vec()))
  }

  /// Clears the buffer
  pub fn reset(&mut self) {
    // don't reset sequence number
    self.buffer.clear();
  }

  pub fn ack(&mut self, seq: u64) -> bool {
    self.buffer.remove(&seq).is_some()
  }

  pub fn push(&mut self, msg: Vec<u8>) -> u64 {
    let seq = self.sequence;
    self.buffer.insert(seq, msg);
    self.sequence += 1;
    seq
  }
}
