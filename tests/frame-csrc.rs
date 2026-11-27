use std::time::{Duration, Instant};

use str0m::format::Codec;
use str0m::media::{MediaKind, MediaTime, Mid};
use str0m::rtp::{RtpHeader, Ssrc};
use str0m::{DATAGRAM_MTU_TARGET, Event, Rtc, RtcError};

mod common;
use common::{TestRtc, connect_l_r_with_rtc, init_crypto_default, init_log, progress};

/// L writes frames through the `Writer`, R reads the RTP packets they become.
fn frame_to_rtp(kind: MediaKind, codec: Codec) -> (TestRtc, TestRtc, Mid, Ssrc) {
    init_log();
    init_crypto_default();

    let now = Instant::now();
    let rtc_l = Rtc::builder().build(now);
    let rtc_r = Rtc::builder().set_rtp_mode(true).build(now);

    let (mut l, mut r) = connect_l_r_with_rtc(rtc_l, rtc_r);

    let mid = "csrc".into();
    let ssrc: Ssrc = 1.into();

    l.direct_api().declare_media(mid, kind);
    l.direct_api().declare_stream_tx(ssrc, None, mid, None);
    r.direct_api().declare_media(mid, kind);
    r.direct_api().expect_stream_rx(ssrc, None, mid, None);

    let max = l.last.max(r.last);
    l.last = max;
    r.last = max;

    assert!(
        l.rtc.codec_config().iter().any(|p| p.spec().codec == codec),
        "{codec:?} is configured"
    );

    (l, r, mid, ssrc)
}

/// Writes `frames` at 20 ms intervals with `csrc`, and returns the header and size of each
/// RTP packet R receives.
fn send(
    l: &mut TestRtc,
    r: &mut TestRtc,
    mid: Mid,
    codec: Codec,
    frames: &[Vec<u8>],
    csrc: &[u32],
) -> Result<Vec<(RtpHeader, usize)>, RtcError> {
    let pt = l
        .rtc
        .codec_config()
        .find(|p| p.spec().codec == codec)
        .map(|p| p.pt())
        .expect("payload type");

    let mut written = 0;
    let mut write_at = l.last + Duration::from_millis(20);

    while l.duration() < Duration::from_secs(2) {
        if written < frames.len() && l.start + l.duration() > write_at {
            write_at = l.last + Duration::from_millis(20);
            let wallclock = l.start + l.duration();
            let time: MediaTime = l.duration().into();
            l.writer(mid).unwrap().csrc(csrc).write(
                pt,
                wallclock,
                time,
                frames[written].clone(),
            )?;
            written += 1;
        }
        progress(l, r)?;
    }
    assert_eq!(written, frames.len(), "all frames written");

    Ok(r.events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::RtpPacket(p) => Some((p.header.clone(), p.header.header_len + p.payload.len())),
            _ => None,
        })
        .collect())
}

#[test]
pub fn frame_csrc_audio() -> Result<(), RtcError> {
    let (mut l, mut r, mid, ssrc) = frame_to_rtp(MediaKind::Audio, Codec::Opus);
    let frames = vec![vec![1u8; 80]; 5];
    let csrc = [u32::MAX, 0x12_34_56_78, 1];

    let packets = send(&mut l, &mut r, mid, Codec::Opus, &frames, &csrc)?;

    assert_eq!(packets.len(), 5, "one packet per frame");
    for (h, _) in &packets {
        assert_eq!(h.ssrc, ssrc);
        assert_eq!(h.csrc_count, 3);
        assert_eq!(&h.csrc[..3], &csrc);
        assert!(h.csrc[3..].iter().all(|c| *c == 0));
    }

    Ok(())
}

/// The packetizer leaves room for the CSRCs, so a frame split to fill its packets makes packets
/// no larger with CSRCs than without.
#[test]
pub fn frame_csrc_video_packets_no_larger() -> Result<(), RtcError> {
    // Each frame is split over several packets, each as large as the budget allows.
    let frames = vec![vec![0u8; 4 * DATAGRAM_MTU_TARGET]; 2];
    let csrc: Vec<u32> = (1..=15).collect();

    let (mut l, mut r, mid, _) = frame_to_rtp(MediaKind::Video, Codec::Vp8);
    let with = send(&mut l, &mut r, mid, Codec::Vp8, &frames, &csrc)?;
    let (mut l, mut r, mid, _) = frame_to_rtp(MediaKind::Video, Codec::Vp8);
    let without = send(&mut l, &mut r, mid, Codec::Vp8, &frames, &[])?;

    assert!(with.len() > frames.len(), "frames split over packets");
    for (h, _) in &with {
        assert_eq!(h.csrc_count, 15);
        assert_eq!(&h.csrc[..], &csrc[..]);
    }
    let largest = |packets: &[(RtpHeader, usize)]| packets.iter().map(|(_, size)| *size).max();
    assert!(
        largest(&with) <= largest(&without),
        "packets with CSRCs are no larger"
    );

    Ok(())
}
