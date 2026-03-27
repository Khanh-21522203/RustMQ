//! Hand-rolled SBE codec for `raft::eraftpb::Message`.
//!
//! Wire format per message (all integers little-endian):
//!
//! ```text
//! ┌─────────────────────────────────────────────────────┐
//! │  SBE header (8 bytes)                               │
//! │    block_length : u16  (= FIXED_BLOCK_LEN)          │
//! │    template_id  : u16  (= 1)                        │
//! │    schema_id    : u16  (= 1)                        │
//! │    version      : u16  (= 1)                        │
//! ├─────────────────────────────────────────────────────┤
//! │  Fixed block (FIXED_BLOCK_LEN bytes)                │
//! │    msg_type         : u32                           │
//! │    to               : u64                           │
//! │    from             : u64                           │
//! │    term             : u64                           │
//! │    log_term         : u64                           │
//! │    index            : u64                           │
//! │    commit           : u64                           │
//! │    commit_term      : u64                           │
//! │    request_snapshot : u64                           │
//! │    reject           : u8  (0=false, 1=true)         │
//! │    reject_hint      : u64                           │
//! │    priority         : i64                           │
//! │    num_entries      : u32                           │
//! │    context_len      : u32                           │
//! │    snapshot_len     : u32                           │
//! ├─────────────────────────────────────────────────────┤
//! │  Variable section                                   │
//! │    context bytes    (context_len)                   │
//! │    snapshot bytes   (snapshot_len, prost-encoded)   │
//! │    entries[]                                        │
//! │      entry_type     : u32                           │
//! │      entry_term     : u64                           │
//! │      entry_index    : u64                           │
//! │      data_len       : u32                           │
//! │      ctx_len        : u32                           │
//! │      data bytes     (data_len)                      │
//! │      ctx bytes      (ctx_len)                       │
//! └─────────────────────────────────────────────────────┘
//! ```
//!
//! Snapshot fields that exist only in some eraftpb versions are encoded as
//! prost bytes in the `snapshot_len` blob to avoid per-version coupling.

use anyhow::{bail, Context, Result};
use bytes::{BufMut, BytesMut};
use prost_raft::Message as ProstEncode; // prost 0.11 used by raft crate
use raft::eraftpb::{Entry, Message, Snapshot};

const TEMPLATE_ID: u16 = 1;
const SCHEMA_ID: u16 = 1;
const VERSION: u16 = 1;

// Fixed block layout offsets (after the 8-byte SBE header)
const OFF_MSG_TYPE: usize = 0;         // u32  4
const OFF_TO: usize = 4;               // u64  8
const OFF_FROM: usize = 12;            // u64  8
const OFF_TERM: usize = 20;            // u64  8
const OFF_LOG_TERM: usize = 28;        // u64  8
const OFF_INDEX: usize = 36;           // u64  8
const OFF_COMMIT: usize = 44;          // u64  8
const OFF_COMMIT_TERM: usize = 52;     // u64  8
const OFF_REQUEST_SNAP: usize = 60;    // u64  8
const OFF_REJECT: usize = 68;          // u8   1
const OFF_REJECT_HINT: usize = 69;     // u64  8
const OFF_PRIORITY: usize = 77;        // i64  8
const OFF_NUM_ENTRIES: usize = 85;     // u32  4
const OFF_CONTEXT_LEN: usize = 89;     // u32  4
const OFF_SNAPSHOT_LEN: usize = 93;    // u32  4
const FIXED_BLOCK_LEN: usize = 97;

const SBE_HEADER_LEN: usize = 8;
const MIN_MSG_LEN: usize = SBE_HEADER_LEN + FIXED_BLOCK_LEN;

// Per-entry fixed prefix: type(4) + term(8) + index(8) + data_len(4) + ctx_len(4) = 28
const ENTRY_HEADER_LEN: usize = 28;

// ── Encode ────────────────────────────────────────────────────────────────────

/// Encode `msg` into `buf` (appending).  The caller is responsible for the
/// 4-byte TCP length-prefix framing; this function writes only the SBE payload.
pub fn encode(msg: &Message, buf: &mut BytesMut) -> Result<()> {
    // Encode snapshot if present (prost, not SBE — snapshot is rare/large)
    let snap_bytes: Vec<u8> = if let Some(snap) = msg.snapshot.as_ref() {
        if !snap.is_empty() {
            ProstEncode::encode_to_vec(snap)
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    let context_len = msg.context.len() as u32;
    let snapshot_len = snap_bytes.len() as u32;
    let num_entries = msg.entries.len() as u32;

    // Calculate total variable length so we can reserve upfront
    let var_len: usize = msg.context.len()
        + snap_bytes.len()
        + msg
            .entries
            .iter()
            .map(|e| ENTRY_HEADER_LEN + e.data.len() + e.context.len())
            .sum::<usize>();

    buf.reserve(MIN_MSG_LEN + var_len);

    // ── SBE header ──
    buf.put_u16_le(FIXED_BLOCK_LEN as u16);
    buf.put_u16_le(TEMPLATE_ID);
    buf.put_u16_le(SCHEMA_ID);
    buf.put_u16_le(VERSION);

    // ── Fixed block ──
    // We write via a temporary slice to match SBE offset layout exactly.
    let mut fixed = [0u8; FIXED_BLOCK_LEN];
    write_u32_le(&mut fixed, OFF_MSG_TYPE, msg.msg_type as u32);
    write_u64_le(&mut fixed, OFF_TO, msg.to);
    write_u64_le(&mut fixed, OFF_FROM, msg.from);
    write_u64_le(&mut fixed, OFF_TERM, msg.term);
    write_u64_le(&mut fixed, OFF_LOG_TERM, msg.log_term);
    write_u64_le(&mut fixed, OFF_INDEX, msg.index);
    write_u64_le(&mut fixed, OFF_COMMIT, msg.commit);
    write_u64_le(&mut fixed, OFF_COMMIT_TERM, msg.commit_term);
    write_u64_le(&mut fixed, OFF_REQUEST_SNAP, msg.request_snapshot);
    fixed[OFF_REJECT] = msg.reject as u8;
    write_u64_le(&mut fixed, OFF_REJECT_HINT, msg.reject_hint);
    write_i64_le(&mut fixed, OFF_PRIORITY, msg.priority);
    write_u32_le(&mut fixed, OFF_NUM_ENTRIES, num_entries);
    write_u32_le(&mut fixed, OFF_CONTEXT_LEN, context_len);
    write_u32_le(&mut fixed, OFF_SNAPSHOT_LEN, snapshot_len);
    buf.put_slice(&fixed);

    // ── Variable section ──
    buf.put_slice(&msg.context);
    buf.put_slice(&snap_bytes);
    for entry in &msg.entries {
        encode_entry(entry, buf);
    }

    Ok(())
}

fn encode_entry(e: &Entry, buf: &mut BytesMut) {
    buf.put_u32_le(e.entry_type as u32);
    buf.put_u64_le(e.term);
    buf.put_u64_le(e.index);
    buf.put_u32_le(e.data.len() as u32);
    buf.put_u32_le(e.context.len() as u32);
    buf.put_slice(&e.data);
    buf.put_slice(&e.context);
}

// ── Decode ────────────────────────────────────────────────────────────────────

/// Decode an SBE payload (without the 4-byte TCP length prefix) into a `Message`.
pub fn decode(src: &[u8]) -> Result<Message> {
    if src.len() < MIN_MSG_LEN {
        bail!(
            "SBE decode: buffer too short ({} < {})",
            src.len(),
            MIN_MSG_LEN
        );
    }

    // ── SBE header ──
    let block_length = u16::from_le_bytes(src[0..2].try_into().unwrap()) as usize;
    let _template_id = u16::from_le_bytes(src[2..4].try_into().unwrap());
    let _schema_id = u16::from_le_bytes(src[4..6].try_into().unwrap());
    let _version = u16::from_le_bytes(src[6..8].try_into().unwrap());

    if block_length < FIXED_BLOCK_LEN {
        bail!("SBE decode: block_length {} < expected {}", block_length, FIXED_BLOCK_LEN);
    }

    let fixed = &src[SBE_HEADER_LEN..SBE_HEADER_LEN + FIXED_BLOCK_LEN];

    let msg_type = read_u32_le(fixed, OFF_MSG_TYPE) as i32;
    let to = read_u64_le(fixed, OFF_TO);
    let from = read_u64_le(fixed, OFF_FROM);
    let term = read_u64_le(fixed, OFF_TERM);
    let log_term = read_u64_le(fixed, OFF_LOG_TERM);
    let index = read_u64_le(fixed, OFF_INDEX);
    let commit = read_u64_le(fixed, OFF_COMMIT);
    let commit_term = read_u64_le(fixed, OFF_COMMIT_TERM);
    let request_snapshot = read_u64_le(fixed, OFF_REQUEST_SNAP);
    let reject = fixed[OFF_REJECT] != 0;
    let reject_hint = read_u64_le(fixed, OFF_REJECT_HINT);
    let priority = read_i64_le(fixed, OFF_PRIORITY);
    let num_entries = read_u32_le(fixed, OFF_NUM_ENTRIES) as usize;
    let context_len = read_u32_le(fixed, OFF_CONTEXT_LEN) as usize;
    let snapshot_len = read_u32_le(fixed, OFF_SNAPSHOT_LEN) as usize;

    // ── Variable section ──
    let mut cursor = SBE_HEADER_LEN + block_length;

    let context = read_bytes(src, &mut cursor, context_len)
        .context("SBE decode: context")?
        .to_vec();

    let snapshot = if snapshot_len > 0 {
        let snap_raw = read_bytes(src, &mut cursor, snapshot_len)
            .context("SBE decode: snapshot")?;
        let s = <Snapshot as ProstEncode>::decode(snap_raw).context("SBE decode: prost snapshot")?;
        Some(s)
    } else {
        None
    };

    let mut entries = Vec::with_capacity(num_entries);
    for i in 0..num_entries {
        let entry = decode_entry(src, &mut cursor)
            .with_context(|| format!("SBE decode: entry {}", i))?;
        entries.push(entry);
    }

    let mut msg = Message::default();
    msg.msg_type = msg_type;
    msg.to = to;
    msg.from = from;
    msg.term = term;
    msg.log_term = log_term;
    msg.index = index;
    msg.commit = commit;
    msg.commit_term = commit_term;
    msg.request_snapshot = request_snapshot;
    msg.reject = reject;
    msg.reject_hint = reject_hint;
    msg.priority = priority;
    msg.context = context;
    msg.snapshot = snapshot.into();
    msg.entries = entries;

    Ok(msg)
}

fn decode_entry(src: &[u8], cursor: &mut usize) -> Result<Entry> {
    if *cursor + ENTRY_HEADER_LEN > src.len() {
        bail!("SBE decode entry: header truncated");
    }
    let entry_type = u32::from_le_bytes(src[*cursor..*cursor + 4].try_into().unwrap()) as i32;
    let term = u64::from_le_bytes(src[*cursor + 4..*cursor + 12].try_into().unwrap());
    let index = u64::from_le_bytes(src[*cursor + 12..*cursor + 20].try_into().unwrap());
    let data_len = u32::from_le_bytes(src[*cursor + 20..*cursor + 24].try_into().unwrap()) as usize;
    let ctx_len = u32::from_le_bytes(src[*cursor + 24..*cursor + 28].try_into().unwrap()) as usize;
    *cursor += ENTRY_HEADER_LEN;

    let data = read_bytes(src, cursor, data_len)?.to_vec();
    let context = read_bytes(src, cursor, ctx_len)?.to_vec();

    let mut entry = Entry::default();
    entry.entry_type = entry_type;
    entry.term = term;
    entry.index = index;
    entry.data = data;
    entry.context = context;
    Ok(entry)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn read_bytes<'a>(src: &'a [u8], cursor: &mut usize, len: usize) -> Result<&'a [u8]> {
    if *cursor + len > src.len() {
        bail!("SBE decode: need {} bytes at offset {}, have {}", len, *cursor, src.len());
    }
    let slice = &src[*cursor..*cursor + len];
    *cursor += len;
    Ok(slice)
}

#[inline] fn write_u32_le(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
#[inline] fn write_u64_le(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
#[inline] fn write_i64_le(buf: &mut [u8], off: usize, v: i64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
#[inline] fn read_u32_le(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}
#[inline] fn read_u64_le(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}
#[inline] fn read_i64_le(buf: &[u8], off: usize) -> i64 {
    i64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use raft::eraftpb::{ConfState, Entry, EntryType, MessageType, Snapshot, SnapshotMetadata};

    fn round_trip(msg: Message) -> Message {
        let mut buf = BytesMut::new();
        encode(&msg, &mut buf).expect("encode");
        decode(&buf).expect("decode")
    }

    #[test]
    fn heartbeat_round_trip() {
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgHeartbeat as i32;
        msg.to = 2;
        msg.from = 1;
        msg.term = 5;
        msg.commit = 42;

        let out = round_trip(msg.clone());
        assert_eq!(out.msg_type, msg.msg_type);
        assert_eq!(out.to, msg.to);
        assert_eq!(out.from, msg.from);
        assert_eq!(out.term, msg.term);
        assert_eq!(out.commit, msg.commit);
    }

    #[test]
    fn vote_round_trip() {
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgRequestVote as i32;
        msg.to = 3;
        msg.from = 1;
        msg.term = 10;
        msg.log_term = 9;
        msg.index = 100;
        msg.reject = false;

        let out = round_trip(msg.clone());
        assert_eq!(out.msg_type, msg.msg_type);
        assert_eq!(out.index, msg.index);
        assert_eq!(out.log_term, msg.log_term);
        assert_eq!(out.reject, msg.reject);
    }

    #[test]
    fn vote_response_reject_round_trip() {
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgRequestVoteResponse as i32;
        msg.to = 1;
        msg.from = 2;
        msg.term = 10;
        msg.reject = true;
        msg.reject_hint = 99;

        let out = round_trip(msg.clone());
        assert_eq!(out.reject, true);
        assert_eq!(out.reject_hint, 99);
    }

    #[test]
    fn append_entries_round_trip() {
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgAppend as i32;
        msg.to = 2;
        msg.from = 1;
        msg.term = 3;
        msg.commit = 7;

        for i in 0..10u64 {
            let mut e = Entry::default();
            e.entry_type = EntryType::EntryNormal as i32;
            e.term = 3;
            e.index = i + 1;
            e.data = format!("cmd-{}", i).into_bytes();
            msg.entries.push(e);
        }

        let out = round_trip(msg.clone());
        assert_eq!(out.entries.len(), 10);
        for (i, e) in out.entries.iter().enumerate() {
            assert_eq!(e.index, i as u64 + 1);
            assert_eq!(e.data, format!("cmd-{}", i).into_bytes());
        }
    }

    #[test]
    fn context_round_trip() {
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgAppend as i32;
        msg.context = b"some-ctx-data".to_vec();

        let out = round_trip(msg.clone());
        assert_eq!(out.context, b"some-ctx-data");
    }

    #[test]
    fn empty_snapshot_round_trip() {
        // No snapshot set — should decode to empty Snapshot
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgHeartbeat as i32;
        msg.to = 2;
        msg.from = 1;

        let out = round_trip(msg);
        assert!(out.snapshot.map_or(true, |s| s.is_empty()));
    }

    #[test]
    fn append_response_round_trip() {
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgAppendResponse as i32;
        msg.to = 1;
        msg.from = 2;
        msg.term = 4;
        msg.index = 55;
        msg.reject = true;
        msg.reject_hint = 50;
        msg.log_term = 3;

        let out = round_trip(msg.clone());
        assert_eq!(out.msg_type, MessageType::MsgAppendResponse as i32);
        assert_eq!(out.index, 55);
        assert_eq!(out.reject, true);
        assert_eq!(out.reject_hint, 50);
        assert_eq!(out.log_term, 3);
    }

    #[test]
    fn heartbeat_response_round_trip() {
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgHeartbeatResponse as i32;
        msg.to = 1;
        msg.from = 3;
        msg.term = 7;
        msg.commit = 100;
        msg.context = b"read-ctx".to_vec();

        let out = round_trip(msg.clone());
        assert_eq!(out.msg_type, MessageType::MsgHeartbeatResponse as i32);
        assert_eq!(out.commit, 100);
        assert_eq!(out.context, b"read-ctx");
    }

    #[test]
    fn pre_vote_round_trip() {
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgRequestPreVote as i32;
        msg.to = 2;
        msg.from = 1;
        msg.term = 11;
        msg.log_term = 10;
        msg.index = 200;

        let out = round_trip(msg.clone());
        assert_eq!(out.msg_type, MessageType::MsgRequestPreVote as i32);
        assert_eq!(out.term, 11);
        assert_eq!(out.log_term, 10);
        assert_eq!(out.index, 200);
    }

    #[test]
    fn pre_vote_response_round_trip() {
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgRequestPreVoteResponse as i32;
        msg.to = 1;
        msg.from = 2;
        msg.term = 11;
        msg.reject = false; // granted

        let out = round_trip(msg.clone());
        assert_eq!(out.msg_type, MessageType::MsgRequestPreVoteResponse as i32);
        assert_eq!(out.reject, false);

        // Also test rejected pre-vote
        let mut msg2 = msg.clone();
        msg2.reject = true;
        msg2.reject_hint = 9;
        let out2 = round_trip(msg2);
        assert_eq!(out2.reject, true);
        assert_eq!(out2.reject_hint, 9);
    }

    #[test]
    fn snapshot_with_data_round_trip() {
        let mut meta = SnapshotMetadata::default();
        meta.index = 500;
        meta.term = 8;
        let mut cs = ConfState::default();
        cs.voters = vec![1, 2, 3];
        meta.conf_state = Some(cs).into();

        let mut snap = Snapshot::default();
        snap.data = b"snapshot-payload-bytes".to_vec();
        snap.metadata = Some(meta).into();

        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgSnapshot as i32;
        msg.to = 2;
        msg.from = 1;
        msg.term = 8;
        msg.snapshot = Some(snap).into();

        let out = round_trip(msg);
        assert_eq!(out.msg_type, MessageType::MsgSnapshot as i32);
        let out_snap = out.snapshot.unwrap();
        assert_eq!(out_snap.data, b"snapshot-payload-bytes");
        let out_meta = out_snap.metadata.unwrap();
        assert_eq!(out_meta.index, 500);
        assert_eq!(out_meta.term, 8);
        assert_eq!(out_meta.conf_state.unwrap().voters, vec![1, 2, 3]);
    }

    #[test]
    fn timeout_now_round_trip() {
        // MsgTimeoutNow: sent by leader to trigger immediate election on target
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgTimeoutNow as i32;
        msg.to = 2;
        msg.from = 1;
        msg.term = 6;

        let out = round_trip(msg.clone());
        assert_eq!(out.msg_type, MessageType::MsgTimeoutNow as i32);
        assert_eq!(out.to, 2);
        assert_eq!(out.term, 6);
    }

    #[test]
    fn transfer_leader_round_trip() {
        // MsgTransferLeader: client asks leader to transfer leadership to node
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgTransferLeader as i32;
        msg.to = 1; // current leader
        msg.from = 2; // target node
        msg.term = 6;

        let out = round_trip(msg.clone());
        assert_eq!(out.msg_type, MessageType::MsgTransferLeader as i32);
        assert_eq!(out.from, 2);
    }

    #[test]
    fn read_index_round_trip() {
        // MsgReadIndex carries the read request ID in context
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgReadIndex as i32;
        msg.to = 1;
        msg.from = 2;
        msg.term = 5;
        msg.context = b"read-req-id-001".to_vec();

        let out = round_trip(msg.clone());
        assert_eq!(out.msg_type, MessageType::MsgReadIndex as i32);
        assert_eq!(out.context, b"read-req-id-001");
    }

    #[test]
    fn read_index_resp_round_trip() {
        // MsgReadIndexResp carries the confirmed index + original request context
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgReadIndexResp as i32;
        msg.to = 2;
        msg.from = 1;
        msg.term = 5;
        msg.index = 300; // confirmed safe-read index
        msg.context = b"read-req-id-001".to_vec();

        let out = round_trip(msg.clone());
        assert_eq!(out.msg_type, MessageType::MsgReadIndexResp as i32);
        assert_eq!(out.index, 300);
        assert_eq!(out.context, b"read-req-id-001");
    }

    #[test]
    fn propose_with_conf_change_entry_round_trip() {
        // MsgPropose with a ConfChange entry (as sent during AddNode/RemoveNode)
        let mut entry = Entry::default();
        entry.entry_type = EntryType::EntryConfChange as i32;
        entry.term = 3;
        entry.index = 42;
        entry.data = b"conf-change-payload".to_vec();
        entry.context = b"cc-ctx".to_vec();

        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgPropose as i32;
        msg.to = 1;
        msg.from = 0; // local propose (from = 0 means local)
        msg.term = 3;
        msg.entries = vec![entry];

        let out = round_trip(msg);
        assert_eq!(out.msg_type, MessageType::MsgPropose as i32);
        assert_eq!(out.entries.len(), 1);
        assert_eq!(out.entries[0].entry_type, EntryType::EntryConfChange as i32);
        assert_eq!(out.entries[0].data, b"conf-change-payload");
        assert_eq!(out.entries[0].context, b"cc-ctx");
    }

    #[test]
    fn hup_beat_check_quorum_round_trip() {
        // Local trigger messages — minimal fields, exercising the zero-value path
        for msg_type in [
            MessageType::MsgHup,
            MessageType::MsgBeat,
            MessageType::MsgCheckQuorum,
        ] {
            let mut msg = Message::default();
            msg.msg_type = msg_type as i32;
            msg.from = 1;
            msg.term = 2;

            let out = round_trip(msg.clone());
            assert_eq!(out.msg_type, msg_type as i32, "failed for {:?}", msg_type);
            assert_eq!(out.from, 1);
            assert_eq!(out.term, 2);
            assert!(out.entries.is_empty());
        }
    }

    #[test]
    fn snap_status_unreachable_round_trip() {
        // MsgSnapStatus and MsgUnreachable: notify leader about follower state
        for msg_type in [MessageType::MsgSnapStatus, MessageType::MsgUnreachable] {
            let mut msg = Message::default();
            msg.msg_type = msg_type as i32;
            msg.to = 1;
            msg.from = 3;
            msg.reject = true; // snap failed / node unreachable

            let out = round_trip(msg.clone());
            assert_eq!(out.msg_type, msg_type as i32, "failed for {:?}", msg_type);
            assert_eq!(out.from, 3);
            assert_eq!(out.reject, true);
        }
    }

    #[test]
    fn all_scalar_fields_preserved() {
        // Single test that every scalar field survives a round-trip without loss.
        let mut msg = Message::default();
        msg.msg_type = MessageType::MsgAppend as i32;
        msg.to = 0xDEAD_BEEF_0001u64;
        msg.from = 0xDEAD_BEEF_0002u64;
        msg.term = 0xDEAD_BEEF_0003u64;
        msg.log_term = 0xDEAD_BEEF_0004u64;
        msg.index = 0xDEAD_BEEF_0005u64;
        msg.commit = 0xDEAD_BEEF_0006u64;
        msg.commit_term = 0xDEAD_BEEF_0007u64;
        msg.request_snapshot = 0xDEAD_BEEF_0008u64;
        msg.reject = true;
        msg.reject_hint = 0xDEAD_BEEF_0009u64;
        msg.priority = -9999;

        let out = round_trip(msg.clone());
        assert_eq!(out.to, msg.to);
        assert_eq!(out.from, msg.from);
        assert_eq!(out.term, msg.term);
        assert_eq!(out.log_term, msg.log_term);
        assert_eq!(out.index, msg.index);
        assert_eq!(out.commit, msg.commit);
        assert_eq!(out.commit_term, msg.commit_term);
        assert_eq!(out.request_snapshot, msg.request_snapshot);
        assert_eq!(out.reject, true);
        assert_eq!(out.reject_hint, msg.reject_hint);
        assert_eq!(out.priority, -9999);
    }
}
