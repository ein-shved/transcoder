use ffmpeg_next::{self as ffmpeg, codec::traits::Decoder, encoder, option::Target};

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

pub fn transcode(input: &Path, output: &Path) -> Result<(), ffmpeg::Error> {
    let mut ictx = ffmpeg::format::input(input)?;
    let mut octx = ffmpeg::format::output(output)?;

    let mut transcoders = Vec::new();

    // Создаем декодеры и энкодеры для каждого потока
    for stream in ictx.streams() {
        let codec = stream.parameters().id();
        let decodec = ffmpeg::decoder::find(codec).ok_or(ffmpeg::Error::DecoderNotFound)?;

        let encodec = ffmpeg::encoder::find(codec).unwrap();
        let mut ost = octx.add_stream(encodec)?;

        // Настраиваем энкодер
        match codec.medium() {
            ffmpeg::media::Type::Video => {
                let encoder = ffmpeg::codec::Context::new_with_codec(encodec).encoder();
                let mut decoder = ffmpeg::codec::Context::new_with_codec(decodec).decoder();
                decoder.set_parameters(stream.parameters().clone())?;

                let video = decoder.video()?;
                let mut evideo = encoder.video()?;
                evideo.set_width(video.width());
                evideo.set_height(video.height());
                evideo.set_format(video.format());
                evideo.set_time_base(video.time_base());
                evideo.set_frame_rate(video.frame_rate());
                ost.set_time_base(video.time_base());

                transcoders.push(Some((
                    Box::new(evideo.open()?) as Box<dyn AsEncoder>,
                    Box::new(video) as Box<dyn DerefMut<Target = ffmpeg::decoder::Opened>>,
                )));
            }
            ffmpeg::media::Type::Audio => {
                let encoder = ffmpeg::codec::Context::new_with_codec(encodec).encoder();
                let mut decoder = ffmpeg::codec::Context::new_with_codec(decodec).decoder();
                decoder.set_parameters(stream.parameters().clone())?;

                let audio = decoder.audio()?;
                let mut eaudio = encoder.audio()?;

                eaudio.set_rate(audio.rate() as i32);
                eaudio.set_channel_layout(audio.channel_layout());
                eaudio.set_format(audio.format());
                eaudio.set_time_base(audio.time_base());
                ost.set_time_base(audio.time_base());

                transcoders.push(Some((
                    Box::new(eaudio.open()?) as Box<dyn AsEncoder>,
                    Box::new(audio) as Box<dyn DerefMut<Target = ffmpeg::decoder::Opened>>,
                )));
            }
            _ => {
                ost.set_parameters(stream.parameters().clone());
                transcoders.push(None);
            }
        }
    }

    octx.write_header()?;

    // Перекодировка
    for (stream, packet) in ictx.packets() {
        let transcoder = &mut transcoders[stream.index()];
        if transcoder.is_none() {
            let mut opacket = packet.clone();
            opacket.set_stream(stream.index());
            opacket.rescale_ts(
                stream.time_base(),
                octx.stream(stream.index()).unwrap().time_base(),
            );
            opacket.write(&mut octx)?;
            continue;
        };
        let (encoder, decoder) = transcoder.as_mut().unwrap();

        let encoder = encoder.as_encoder();

        decoder.send_packet(&packet)?;

        let mut frame = unsafe { ffmpeg::Frame::empty() };

        while decoder.receive_frame(&mut frame).is_ok() {
            encoder.send_frame(&frame)?;
            let mut epacket = ffmpeg::Packet::empty();

            while encoder.receive_packet(&mut epacket).is_ok() {
                epacket.set_stream(stream.index());
                epacket.rescale_ts(
                    decoder.time_base(),
                    octx.stream(stream.index()).unwrap().time_base(),
                );
                epacket.write(&mut octx)?;
            }
        }
    }

    // Flush
    for (i, transcoder) in transcoders.iter_mut().enumerate() {
        if let Some((encoder, decoder)) = transcoder.as_mut() {
            decoder.send_eof()?;
            let mut frame = unsafe { ffmpeg::Frame::empty() };
            let encoder = encoder.as_encoder();

            while decoder.receive_frame(&mut frame).is_ok() {
                encoder.send_frame(&frame)?;
            }

            encoder.send_eof()?;
            let mut epacket = ffmpeg::Packet::empty();
            while encoder.receive_packet(&mut epacket).is_ok() {
                epacket.set_stream(i);
                epacket.write(&mut octx)?;
            }
        }
    }

    octx.write_trailer()?;
    Ok(())
}
