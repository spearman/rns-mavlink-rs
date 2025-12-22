use std::collections::BTreeMap;
use tokio::time::{Duration, Instant};

pub mod fc;
pub mod gc;

pub use fc::Fc;
pub use gc::Gc;

/// Keep track of current seqnum and un-acked messages.
pub(crate) struct MavlinkBuffer {
  pub sequence: Seqnum,
  pub buffer: BTreeMap<Seqnum, Message>,
}

/// Message last sent to peer at ts. When re-sending we will update the ts to reflect
/// the new transmission time.
pub(crate) struct Message {
  pub ts: Instant,
  pub data: Vec<u8>
}

#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct Seqnum(pub u64);

impl MavlinkBuffer {
  pub fn new() -> Self {
    MavlinkBuffer {
      sequence: Seqnum(0),
      buffer: BTreeMap::new()
    }
  }

  /// Clears the buffer leaving sequence number unchanged
  pub fn buffer(&self) -> &BTreeMap<Seqnum, Message> {
    &self.buffer
  }

  /// Clears the buffer leaving sequence number unchanged
  pub fn clear(&mut self) {
    self.buffer.clear();
  }

  pub fn ack(&mut self, seq: Seqnum) -> Option<Message> {
    self.buffer.remove(&seq)
  }

  /// Enqueue the message and return the seqnum and payload [seqnum, data]
  pub fn push(&mut self, data: Vec<u8>) -> (Seqnum, Vec<u8>) {
    let seq = self.sequence;
    let ts = Instant::now();
    let msg = Message { ts, data };
    let payload = msg.payload(seq);
    self.buffer.insert(seq, msg);
    self.sequence.0 += 1;
    (seq, payload)
  }

  /// Given a timeout duration, return any payloads that should be re-transmitted
  pub fn retransmit(&mut self, timeout: Duration) -> Vec<Vec<u8>> {
    let now = Instant::now();
    self.buffer.iter_mut().filter_map(|(seqnum, message)|{
      let retransmit = now - message.ts >= timeout;
      if retransmit {
        message.ts = now;
      }
      Some(message.payload(*seqnum))
    }).collect()
  }
}

impl Message {
  /// Constructs the payload [seqnum, data]
  #[inline]
  pub fn payload(&self, seqnum: Seqnum) -> Vec<u8> {
    [seqnum.0.to_be_bytes().as_slice(), self.data.as_slice()].concat().to_vec()
  }
}
