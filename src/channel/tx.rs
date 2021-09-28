
use super::Fragment;
use super::Payload;
use super::Datagram;
use super::WindowAck;

use super::FRAGMENT_SIZE;
use super::TRANSFER_WINDOW_SIZE;
use super::WINDOW_ACK_SPACING;

use super::seq;

use super::SendMode;

use std::collections::VecDeque;

pub struct Tx {
    // Incrementing sequence id for send queue
    next_sequence_id: seq::Id,
    // The id of the most recently enqueued reliable packet
    last_reliable_id: Option<seq::Id>,
    // Transfer window base
    base_sequence_id: u32,
    // Queue of outbound datagrams
    send_queue: VecDeque<(Datagram, bool)>,
}

impl Tx {
    pub fn new() -> Self {
        Self {
            next_sequence_id: 0,
            last_reliable_id: None,
            base_sequence_id: 0,
            send_queue: VecDeque::new(),
        }
    }

    pub fn enqueue_packet(&mut self, sequence_id: seq::Id, dependent_id: Option<seq::Id>, data: Box<[u8]>, reliable: bool) {
        let num_full_fragments = data.len() / FRAGMENT_SIZE;
        let bytes_remaining = data.len() % FRAGMENT_SIZE;

        let caboose = data.len() == 0 || bytes_remaining != 0;
        let num_fragments = num_full_fragments as u32 + caboose as u32;

        let dependent_lead = dependent_id.map_or(0, |id| seq::lead_unsigned(sequence_id, id) as u16);

        let last_fragment_id = (num_fragments - 1) as u16;

        for i in 0..num_full_fragments {
            let begin = i*FRAGMENT_SIZE;
            let end = (i + 1)*FRAGMENT_SIZE;
            let slice = &data[begin .. end];
            self.send_queue.push_back((
                Datagram {
                    sequence_id: sequence_id,
                    dependent_lead: dependent_lead,
                    payload: Payload::Fragment(Fragment {
                        fragment_id: i as u16,
                        last_fragment_id: last_fragment_id,
                        data: slice.into(),
                    })
                },
                reliable
            ));
        }

        if caboose {
            let begin = num_full_fragments * FRAGMENT_SIZE;
            let slice = &data[begin .. ];
            self.send_queue.push_back((
                Datagram {
                    sequence_id: sequence_id,
                    dependent_lead: dependent_lead,
                    payload: Payload::Fragment(Fragment {
                        fragment_id: last_fragment_id,
                        last_fragment_id: last_fragment_id,
                        data: slice.into(),
                    })
                },
                reliable
            ));
        }
    }

    pub fn enqueue_sentinel(&mut self, sequence_id: seq::Id, dependent_id: Option<seq::Id>) {
        let dependent_lead = dependent_id.map_or(0, |id| seq::lead_unsigned(sequence_id, id) as u16);

        self.send_queue.push_back((
            Datagram {
                sequence_id: sequence_id,
                dependent_lead: dependent_lead,
                payload: Payload::Sentinel,
            },
            true
        ));
    }

    pub fn enqueue(&mut self, data: Box<[u8]>, mode: SendMode) {
        if let Some(last_id) = self.last_reliable_id {
            if seq::lead_unsigned(self.next_sequence_id, last_id) >= TRANSFER_WINDOW_SIZE {
                self.last_reliable_id = None;
            }
        }

        self.enqueue_packet(self.next_sequence_id, self.last_reliable_id, data, mode != SendMode::Unreliable);

        match mode {
            SendMode::Unreliable => {
                if (self.next_sequence_id % WINDOW_ACK_SPACING) == WINDOW_ACK_SPACING - 1 {
                    // Ensure that at a minimum, this packet's sequence information is received
                    self.enqueue_sentinel(self.next_sequence_id, self.last_reliable_id);
                }
            }
            SendMode::Reliable => {
                if mode == SendMode::Reliable {
                    // Future packets will wait for this one
                    self.last_reliable_id = Some(self.next_sequence_id);
                }

            }
            _ => ()
        }

        self.next_sequence_id = seq::add(self.next_sequence_id, 1);
    }

    pub fn try_send(&mut self) -> Option<(Datagram, bool)> {
        if let Some((datagram, _)) = self.send_queue.front() {
            let next_lead = seq::lead_unsigned(datagram.sequence_id, self.base_sequence_id);

            if next_lead < TRANSFER_WINDOW_SIZE {
                return self.send_queue.pop_front();
            }
        }

        None
    }

    pub fn handle_window_ack(&mut self, ack: WindowAck) {
        let new_base_sequence_id = seq::add(ack.sequence_id, 1);
        if seq::lead_unsigned(new_base_sequence_id, self.base_sequence_id) <= TRANSFER_WINDOW_SIZE {
            self.base_sequence_id = new_base_sequence_id;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.send_queue.is_empty()
    }
}

#[test]
fn test_basic_send() {
    let mut tx = Tx::new();

    let p0 = vec![ 0,  1,  2,  3].into_boxed_slice();
    let p1 = vec![ 4,  5,  6,  7].into_boxed_slice();
    let p2 = vec![ 8,  9, 10, 11].into_boxed_slice();

    tx.enqueue(p0.clone(), SendMode::Unreliable);
    tx.enqueue(p1.clone(), SendMode::Reliable);
    tx.enqueue(p2.clone(), SendMode::Passive);

    let (dg0,r0) = tx.try_send().unwrap();
    let (dg1,r1) = tx.try_send().unwrap();
    let (dg2,r2) = tx.try_send().unwrap();

    assert_eq!(tx.try_send(), None);

    assert_eq!(r0, false);
    assert_eq!(r1, true);
    assert_eq!(r2, true);

    assert_eq!(dg0, Datagram {
        sequence_id: 0,
        dependent_lead: 0,
        payload: Payload::Fragment(Fragment {
            fragment_id: 0,
            last_fragment_id: 0,
            data: p0,
        })
    });

    assert_eq!(dg1, Datagram {
        sequence_id: 1,
        dependent_lead: 0,
        payload: Payload::Fragment(Fragment {
            fragment_id: 0,
            last_fragment_id: 0,
            data: p1,
        })
    });

    assert_eq!(dg2, Datagram {
        sequence_id: 2,
        dependent_lead: 1,
        payload: Payload::Fragment(Fragment {
            fragment_id: 0,
            last_fragment_id: 0,
            data: p2,
        })
    });
}

#[test]
fn test_dependents() {
    let mut tx = Tx::new();

    let p0 = vec![ 0,  1,  2,  3].into_boxed_slice();
    tx.enqueue(p0, SendMode::Reliable);

    for _ in 0 .. TRANSFER_WINDOW_SIZE {
        let p1 = vec![ 4,  5,  6,  7].into_boxed_slice();
        tx.enqueue(p1, SendMode::Unreliable);
    }

    while let Some((dg,_)) = tx.try_send() {
        assert_eq!(dg.dependent_lead as u32, dg.sequence_id);
    }

    tx.handle_window_ack(WindowAck{ sequence_id: WINDOW_ACK_SPACING - 1 });

    let (dg,_) = tx.try_send().unwrap();
    assert_eq!(dg.dependent_lead, 0);

    assert_eq!(tx.try_send(), None);
}

#[test]
fn test_sentinels() {
    let mut tx = Tx::new();

    for _ in 0 .. TRANSFER_WINDOW_SIZE {
        tx.enqueue(vec![ 0,  1,  2,  3].into_boxed_slice(), SendMode::Unreliable);
    }

    for i in 0 .. TRANSFER_WINDOW_SIZE {
        let (dg, _) = tx.try_send().unwrap();
        assert_eq!(dg.sequence_id, i);
        match dg.payload {
            Payload::Sentinel => panic!("Sentinel not expected here! sequence_id: {}", dg.sequence_id),
            _ => (),
        }
        if i % WINDOW_ACK_SPACING == WINDOW_ACK_SPACING - 1 {
            let (dg, rel) = tx.try_send().unwrap();
            assert_eq!(rel, true);
            assert_eq!(dg.sequence_id, i);
            match dg.payload {
                Payload::Sentinel => (),
                _ => panic!("Sentinel expected here"),
            }
        }
    }

    assert_eq!(tx.try_send(), None);
}

#[test]
fn test_window_acks() {
    let mut tx = Tx::new();

    for _ in 0 .. 2*TRANSFER_WINDOW_SIZE {
        tx.enqueue(vec![ 0,  1,  2,  3].into_boxed_slice(), SendMode::Unreliable);
    }

    for i in 0 .. TRANSFER_WINDOW_SIZE {
        let (dg, _) = tx.try_send().unwrap();
        assert_eq!(dg.sequence_id, i);
        if i % WINDOW_ACK_SPACING == WINDOW_ACK_SPACING - 1 {
            let (dg, _) = tx.try_send().unwrap();
            assert_eq!(dg.sequence_id, i);
        }
    }

    assert_eq!(tx.try_send(), None);

    for j in 0 .. TRANSFER_WINDOW_SIZE/WINDOW_ACK_SPACING {
        tx.handle_window_ack(WindowAck{ sequence_id: j*WINDOW_ACK_SPACING + WINDOW_ACK_SPACING - 1 });

        for i in 0 .. WINDOW_ACK_SPACING {
            let sequence_id = TRANSFER_WINDOW_SIZE + j*WINDOW_ACK_SPACING + i;
            let (dg, _) = tx.try_send().unwrap();
            assert_eq!(dg.sequence_id, sequence_id);
            if i == WINDOW_ACK_SPACING - 1 {
                let (dg, _) = tx.try_send().unwrap();
                assert_eq!(dg.sequence_id, sequence_id);
            }
        }

        assert_eq!(tx.try_send(), None);
    }

    assert_eq!(tx.try_send(), None);
}

// TODO: Test fragmentation

