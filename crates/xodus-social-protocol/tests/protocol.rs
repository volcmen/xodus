use std::io::Cursor;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use xodus_social_protocol::{
    FrameWriter, FriendHandle, ProtocolError, SocialRequest, decode_frame, encode_frame, read_frame,
};

#[test]
fn snapshot_frame_keeps_the_version_one_wire_representation() {
    assert_eq!(
        encode_frame(&SocialRequest::Snapshot).unwrap(),
        b"XDSO\x01\x00\x0a\x00\x00\x00\"Snapshot\""
    );
}

#[test]
fn social_frames_round_trip_with_version_one() {
    let request = SocialRequest::InviteFriend {
        friend: FriendHandle::new(),
    };
    let bytes = encode_frame(&request).unwrap();

    assert_eq!(&bytes[0..4], b"XDSO");
    assert_eq!(u16::from_le_bytes(bytes[4..6].try_into().unwrap()), 1);
    assert_eq!(decode_frame::<SocialRequest>(&bytes).unwrap(), request);
}

#[tokio::test]
async fn social_oversized_length_is_rejected_before_payload_allocation() {
    let mut bytes = b"XDSO".to_vec();
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());

    let error = read_frame::<_, SocialRequest>(&mut Cursor::new(bytes))
        .await
        .unwrap_err();
    assert!(matches!(error, ProtocolError::FrameTooLarge));
}

#[test]
fn social_unknown_version_and_mismatched_frames_are_rejected() {
    let request = SocialRequest::Snapshot;
    let bytes = encode_frame(&request).unwrap();

    let mut unknown_magic = bytes.clone();
    unknown_magic[..4].copy_from_slice(b"NOPE");
    assert!(matches!(
        decode_frame::<SocialRequest>(&unknown_magic),
        Err(ProtocolError::InvalidMagic)
    ));

    let mut unknown_version = bytes.clone();
    unknown_version[4..6].copy_from_slice(&2_u16.to_le_bytes());
    assert!(matches!(
        decode_frame::<SocialRequest>(&unknown_version),
        Err(ProtocolError::UnsupportedVersion)
    ));

    assert!(matches!(
        decode_frame::<SocialRequest>(&bytes[..9]),
        Err(ProtocolError::Truncated)
    ));

    let mut truncated_payload = bytes.clone();
    truncated_payload.pop();
    assert!(matches!(
        decode_frame::<SocialRequest>(&truncated_payload),
        Err(ProtocolError::LengthMismatch)
    ));

    let mut trailing_payload = bytes;
    trailing_payload.push(0);
    assert!(matches!(
        decode_frame::<SocialRequest>(&trailing_payload),
        Err(ProtocolError::LengthMismatch)
    ));
}

#[tokio::test]
async fn social_one_byte_fragmented_frame_is_read() {
    let request = SocialRequest::JoinFriend {
        friend: FriendHandle::new(),
    };
    let bytes = encode_frame(&request).unwrap();
    let (mut writer, mut reader) = tokio::io::duplex(64);
    let send = tokio::spawn(async move {
        for byte in bytes {
            writer.write_all(&[byte]).await.unwrap();
            tokio::task::yield_now().await;
        }
    });

    let decoded = read_frame::<_, SocialRequest>(&mut reader).await.unwrap();
    send.await.unwrap();
    assert_eq!(decoded, request);
}

#[tokio::test]
async fn social_concurrent_complete_frame_writes_do_not_interleave() {
    let (writer, mut reader) = tokio::io::duplex(256);
    let writer = FrameWriter::new(writer);
    let first = {
        let writer = writer.clone();
        tokio::spawn(async move { writer.write(&SocialRequest::Snapshot).await })
    };
    let second = {
        let writer = writer.clone();
        tokio::spawn(async move { writer.write(&SocialRequest::RefreshFriends).await })
    };

    let first_read = read_frame::<_, SocialRequest>(&mut reader).await.unwrap();
    let second_read = read_frame::<_, SocialRequest>(&mut reader).await.unwrap();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();

    assert_ne!(first_read, second_read);
    assert!(matches!(
        first_read,
        SocialRequest::Snapshot | SocialRequest::RefreshFriends
    ));
    assert!(matches!(
        second_read,
        SocialRequest::Snapshot | SocialRequest::RefreshFriends
    ));

    drop(writer);
    let mut trailing = Vec::new();
    reader.read_to_end(&mut trailing).await.unwrap();
    assert!(trailing.is_empty());
}
