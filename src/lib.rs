use ffmpeg_next::{self as ffmpeg, codec::debug};
use log::{debug, trace};

use std::{ops::DerefMut, path::Path};

trait AsEncoder {
    fn as_encoder(&mut self) -> &mut ffmpeg::encoder::Encoder;
}

impl AsEncoder for ffmpeg::encoder::Audio {
    fn as_encoder(&mut self) -> &mut ffmpeg_next::encoder::Encoder {
        &mut **self
    }
}
impl AsEncoder for ffmpeg::encoder::Video {
    fn as_encoder(&mut self) -> &mut ffmpeg_next::encoder::Encoder {
        &mut **self
    }
}

struct Transcoder {
    ts: Option<(
        Box<dyn AsEncoder>,
        Box<dyn DerefMut<Target = ffmpeg::decoder::Opened>>,
    )>,
    in_time_base: ffmpeg::Rational,
    out_time_base: ffmpeg::Rational,
    index: usize,
    pts_shift: Option<i64>,
    dts_shift: Option<i64>,
}

pub fn transcode(input: &Path, output: &Path) -> Result<(), ffmpeg::Error> {
    ffmpeg_next::init().unwrap();
    // ffmpeg_next::log::set_level(ffmpeg_next::log::Level::Debug);

    let mut ictx = ffmpeg::format::input(input)?;
    let mut octx = ffmpeg::format::output(output)?;

    let mut transcoders = Vec::new();

    // Создаем декодеры и энкодеры для каждого потока
    for stream in ictx.streams() {
        let codec = stream.parameters().id();
        debug!(
            "Stream: {}: {codec:?} of {:?}",
            stream.index(),
            codec.medium()
        );
        let decodec = ffmpeg::decoder::find(codec).ok_or(ffmpeg::Error::DecoderNotFound)?;

        let encodec = ffmpeg::encoder::find(codec).unwrap();
        let global_header = octx
            .format()
            .flags()
            .contains(ffmpeg::format::Flags::GLOBAL_HEADER);

        match codec.medium() {
            ffmpeg::media::Type::Video => {
                let mut ost = octx.add_stream(encodec)?;
                let encoder = ffmpeg::codec::Context::new_with_codec(encodec).encoder();
                let mut decoder = ffmpeg::codec::Context::new_with_codec(decodec).decoder();
                decoder.set_parameters(stream.parameters().clone())?;

                let video = decoder.video()?;
                let mut evideo = encoder.video()?;

                ost.set_parameters(&evideo);

                evideo.set_width(video.width());
                evideo.set_height(video.height());
                evideo.set_aspect_ratio(video.aspect_ratio());
                evideo.set_format(video.format());
                evideo.set_frame_rate(video.frame_rate());

                evideo.set_time_base(if stream.time_base().numerator() > 0 {
                    stream.time_base()
                } else if let Some(frame_rate) = video.frame_rate() {
                    ffmpeg::Rational::new(frame_rate.1, frame_rate.0)
                } else {
                    ffmpeg::Rational::new(1, 25)
                });
                if global_header {
                    evideo.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
                }

                let mut options = ffmpeg::Dictionary::new();
                options.set("preset", "medium");

                let in_time_base = video.time_base();
                let out_time_base = evideo.time_base();
                let evideo = evideo.open_as_with(encodec, options)?;

                ost.set_parameters(&evideo);
                ost.set_time_base(evideo.time_base());
                ost.set_metadata(stream.metadata().to_owned());

                transcoders.push(Transcoder {
                    ts: Some((Box::new(evideo), Box::new(video))),
                    index: ost.index(),
                    in_time_base: in_time_base,
                    out_time_base: out_time_base,
                });
            }
            ffmpeg::media::Type::Audio => {
                let encodec = encodec.audio()?;
                let mut ost = octx.add_stream(encodec)?;
                let encoder = ffmpeg::codec::Context::from_parameters(ost.parameters())?.encoder();
                let mut decoder = ffmpeg::codec::Context::new_with_codec(decodec).decoder();
                decoder.set_parameters(stream.parameters().clone())?;

                let audio = decoder.audio()?;
                let mut eaudio = encoder.audio()?;

                let channel_layout = encodec
                    .audio()?
                    .channel_layouts()
                    .map(|cls| cls.best(audio.channel_layout().channels()))
                    .unwrap_or(ffmpeg::channel_layout::ChannelLayout::STEREO);

                eaudio.set_rate(audio.rate() as i32);
                eaudio.set_channel_layout(channel_layout);
                eaudio.set_format(audio.format());
                eaudio.set_bit_rate(audio.bit_rate());
                eaudio.set_max_bit_rate(audio.max_bit_rate());
                eaudio.set_frame_rate(audio.frame_rate());

                if global_header {
                    eaudio.set_flags(ffmpeg::codec::Flags::GLOBAL_HEADER);
                }

                eaudio.set_time_base(if stream.time_base().numerator() > 0 {
                    stream.time_base()
                } else {
                    ffmpeg::Rational::new(1, audio.rate() as i32)
                });

                let in_time_base = audio.time_base();
                let out_time_base = eaudio.time_base();
                let eaudio = eaudio.open_as(encodec)?;

                ost.set_time_base(eaudio.time_base());
                ost.set_parameters(&eaudio);
                ost.set_metadata(stream.metadata().to_owned());

                transcoders.push(Transcoder {
                    ts: Some((
                        Box::new(eaudio) as Box<dyn AsEncoder>,
                        Box::new(audio) as Box<dyn DerefMut<Target = ffmpeg::decoder::Opened>>,
                    )),
                    index: ost.index(),
                    in_time_base: in_time_base,
                    out_time_base: out_time_base,
                    pts_shift: None,
                    dts_shift: None,
                });
            }
            _ => {
                let mut ost = octx.add_stream(encodec)?;
                ost.set_parameters(stream.parameters().clone());
                transcoders.push(Transcoder {
                    ts: None,
                    index: ost.index(),
                    in_time_base: stream.time_base(),
                    out_time_base: stream.time_base(),
                    pts_shift: None,
                    dts_shift: None,
                });
                ost.set_metadata(stream.metadata().to_owned());
            }
        }
    }

    octx.set_metadata(ictx.metadata().to_owned());
    octx.write_header()?;

    for (stream, mut packet) in ictx.packets() {
        debug!(
            "Packet {} of {} {:?}, {:?}",
            packet.position(),
            stream.index(),
            packet.pts(),
            packet.dts()
        );
        let transcoder = &mut transcoders[stream.index()];
        if transcoder.ts.is_none() {
            packet.rescale_ts(stream.time_base(), transcoder.out_time_base);
            packet.set_stream(stream.index());
            packet.set_dts(make_shift(packet.dts(), &mut transcoder.dts_shift));
            let need_to_rescale = transcoder.pts_shift.is_none() && transcoder.dts_shift.is_some();
            packet.set_pts(make_shift(packet.pts(), &mut transcoder.pts_shift));
            if transcoder.pts_shift.is_some() && need_to_rescale {
                transcoder.pts_shift =
                    Some(transcoder.pts_shift.unwrap() + transcoder.dts_shift.unwrap());
                packet.set_pts(Some(packet.pts().unwrap() + transcoder.dts_shift.unwrap()));
            }
            debug!(
                "Epacket {} of {} {:?}, {:?}",
                packet.position(),
                stream.index(),
                packet.pts(),
                packet.dts()
            );
            packet.write(&mut octx)?;
            continue;
        };
        packet.rescale_ts(stream.time_base(), transcoder.in_time_base);
        let (encoder, decoder) = transcoder.ts.as_mut().unwrap();

        let encoder = encoder.as_encoder();

        decoder.send_packet(&packet)?;

        let mut frame = unsafe { ffmpeg::Frame::empty() };

        while decoder.receive_frame(&mut frame).is_ok() {
            frame.set_pts(frame.timestamp());
            encoder.send_frame(&frame)?;
            let mut epacket = ffmpeg::Packet::empty();

            while encoder.receive_packet(&mut epacket).is_ok() {
                epacket.set_stream(transcoder.index);
                epacket.rescale_ts(transcoder.in_time_base, transcoder.out_time_base);
                epacket.set_dts(make_shift(epacket.dts(), &mut transcoder.dts_shift));
                let need_to_rescale =
                    transcoder.pts_shift.is_none() && transcoder.dts_shift.is_some();
                epacket.set_pts(make_shift(epacket.pts(), &mut transcoder.pts_shift));
                if transcoder.pts_shift.is_some() && need_to_rescale {
                    transcoder.pts_shift =
                        Some(transcoder.pts_shift.unwrap() + transcoder.dts_shift.unwrap());
                }
                debug!(
                    "Epacket {} of {} {:?}, {:?}",
                    epacket.position(),
                    transcoder.index,
                    epacket.pts(),
                    epacket.dts()
                );
                epacket.write(&mut octx)?;
            }
        }
    }

    // Flush
    for transcoder in transcoders.iter_mut() {
        if let Some((encoder, decoder)) = transcoder.ts.as_mut() {
            decoder.send_eof()?;
            let mut frame = unsafe { ffmpeg::Frame::empty() };
            let encoder = encoder.as_encoder();

            while decoder.receive_frame(&mut frame).is_ok() {
                frame.set_pts(frame.timestamp());
                encoder.send_frame(&frame)?;
            }

            encoder.send_eof()?;
            let mut epacket = ffmpeg::Packet::empty();
            while encoder.receive_packet(&mut epacket).is_ok() {
                epacket.set_stream(transcoder.index);
                epacket.rescale_ts(transcoder.in_time_base, transcoder.out_time_base);
                epacket.set_pts(make_shift(epacket.pts(), &mut transcoder.pts_shift));
                epacket.set_dts(make_shift(epacket.dts(), &mut transcoder.dts_shift));
                debug!(
                    "Epacket {} of {} {:?}, {:?}",
                    epacket.position(),
                    transcoder.index,
                    epacket.pts(),
                    epacket.dts()
                );
                epacket.write(&mut octx)?;
            }
        }
    }

    octx.write_trailer()?;
    Ok(())
}

fn make_shift(cur: Option<i64>, shift: &mut Option<i64>) -> Option<i64> {
    if let Some(cur) = cur {
        if let Some(shift) = shift {
            debug!("Making shift from {} to {}", cur, cur + *shift);
            Some(cur + *shift)
        } else {
            debug!("Making shift to {}", -cur);
            *shift = Some(-cur);
            Some(0)
        }
    } else {
        cur
    }
}
