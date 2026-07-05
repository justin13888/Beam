//! Example program that extracts and displays metadata from a media file.

use beam_server::utils::metadata::{StreamMetadata, VideoFileMetadata};
use ffmpeg_next as ffmpeg;

use std::{env, path::Path};

fn main() -> Result<(), eyre::Error> {
    color_eyre::install()?;
    dotenvy::dotenv().ok();
    beam_server::logging::init_tracing();

    ffmpeg::init().unwrap();

    let file_path = env::args().nth(1).expect("missing file");
    let metadata = VideoFileMetadata::from_path(Path::new(&file_path))?;

    println!("=== FILE INFORMATION ===");
    println!(
        "Format: {} ({})",
        metadata.format_name, metadata.format_long_name
    );
    println!(
        "File size: {:.2} GB",
        metadata.file_size as f64 / 1_000_000_000.0
    );
    println!(
        "Duration: {:.2} seconds ({:.0}:{:02.0}:{:02.0})",
        metadata.duration_seconds(),
        metadata.duration_seconds() / 3600.0,
        (metadata.duration_seconds() % 3600.0) / 60.0,
        metadata.duration_seconds() % 60.0
    );
    println!(
        "Bit rate: {:.1} Mbps",
        metadata.bit_rate as f64 / 1_000_000.0
    );
    println!("Probe score: {}", metadata.probe_score);

    // Print file-level metadata
    if !metadata.metadata.is_empty() {
        println!("\n=== FILE METADATA ===");
        for (k, v) in &metadata.metadata {
            println!("{}: {}", k, v);
        }
    }

    // Print best streams
    println!("\n=== BEST STREAMS ===");
    if let Some(index) = metadata.best_video_stream {
        println!("Best video stream: {}", index);
    }

    if let Some(index) = metadata.best_audio_stream {
        println!("Best audio stream: {}", index);
    }

    if let Some(index) = metadata.best_subtitle_stream {
        println!("Best subtitle stream: {}", index);
    }

    // Print detailed stream information
    println!("\n=== STREAM DETAILS ===");
    for stream in &metadata.streams {
        match stream {
            StreamMetadata::Video(video_stream) => {
                println!("\nStream #{} (Video):", video_stream.index);
                println!("\tCodec: {:?}", video_stream.codec_id);
                println!("\tTime base: {}", video_stream.time_base);
                println!("\tStart time: {}", video_stream.start_time);
                let actual_duration =
                    video_stream.actual_duration_seconds(metadata.duration_seconds());
                println!("\tDuration: {:.2} seconds", actual_duration);

                let actual_frames = video_stream.actual_frames();
                println!("\tFrames: {}", actual_frames);
                println!(
                    "\tFrame rate: {:.3} fps",
                    video_stream.frame_rate().unwrap_or(0.0)
                );
                println!("\tDisposition: {:?}", video_stream.disposition);
                println!("\tDiscard: {:?}", video_stream.discard);

                // Print stream metadata
                if !video_stream.metadata.is_empty() {
                    println!("\tStream Metadata:");
                    for (k, v) in &video_stream.metadata {
                        println!("\t\t{}: {}", k, v);
                    }
                }

                let video = &video_stream.video;
                println!("\t=== VIDEO PROPERTIES ===");
                println!(
                    "\tCodec: {} (Profile: {}, Level: {})",
                    video.codec_name, video.profile, video.level
                );
                println!(
                    "\tResolution: {}x{} (Aspect ratio: {})",
                    video.width, video.height, video.aspect_ratio
                );
                println!(
                    "\tPixel format: {:?} ({}-bit)",
                    video.format,
                    video.bit_depth().unwrap_or(0)
                );
                let actual_bitrate = video.actual_bit_rate(&video_stream.metadata);
                println!("\tBit rate: {:.1} Mbps", actual_bitrate / 1_000_000.0);
                println!("\tDelay: {}", video.delay);
                println!("\tB-frames: {}", video.has_b_frames);
                println!("\tColor space: {:?}", video.color_space);
                println!("\tColor range: {:?}", video.color_range);
                println!("\tColor primaries: {:?}", video.color_primaries);
                println!(
                    "\tTransfer characteristic: {:?}",
                    video.color_transfer_characteristic
                );
                println!("\tChroma location: {:?}", video.chroma_location);
                println!("\tReference frames: {}", video.references);
                println!("\tIntra DC precision: {}", video.intra_dc_precision);
            }
            StreamMetadata::Audio(audio_stream) => {
                println!("\nStream #{} (Audio):", audio_stream.index);
                println!("\tCodec: {:?}", audio_stream.codec_id);
                println!("\tTime base: {}", audio_stream.time_base);
                println!("\tStart time: {}", audio_stream.start_time);
                let actual_duration =
                    audio_stream.actual_duration_seconds(metadata.duration_seconds());
                println!("\tDuration: {:.2} seconds", actual_duration);

                let actual_frames = audio_stream.actual_frames();
                println!("\tFrames: {}", actual_frames);
                println!("\tDisposition: {:?}", audio_stream.disposition);
                println!("\tDiscard: {:?}", audio_stream.discard);

                // Print stream metadata
                if !audio_stream.metadata.is_empty() {
                    println!("\tStream Metadata:");
                    for (k, v) in &audio_stream.metadata {
                        println!("\t\t{}: {}", k, v);
                    }
                }

                let audio = &audio_stream.audio;
                println!("\t=== AUDIO PROPERTIES ===");
                println!("\tCodec: {} (Profile: {})", audio.codec_name, audio.profile);
                if !audio.title.is_empty() {
                    println!("\tTitle: {}", audio.title);
                }
                if !audio.language.is_empty() {
                    println!("\tLanguage: {}", audio.language);
                }
                println!("\tSample rate: {} Hz", audio.rate);
                println!(
                    "\tChannels: {} ({})",
                    audio.channels,
                    audio.channel_layout_description()
                );
                println!("\tSample format: {:?}", audio.format);
                let actual_bitrate = audio.actual_bit_rate(&audio_stream.metadata);
                println!("\tBit rate: {:.1} kbps", actual_bitrate / 1000.0);
                println!("\tMax bit rate: {} kbps", audio.max_rate / 1000);
                let actual_audio_frames = audio.actual_frames(&audio_stream.metadata);
                println!("\tFrames: {}", actual_audio_frames);
                println!("\tAlign: {}", audio.align);
                println!("\tDelay: {}", audio.delay);
            }
            StreamMetadata::Subtitle(subtitle_stream) => {
                println!("\nStream #{} (Subtitle):", subtitle_stream.index);
                println!("\tCodec: {:?}", subtitle_stream.codec_id);
                println!("\tTime base: {}", subtitle_stream.time_base);
                println!("\tStart time: {}", subtitle_stream.start_time);
                let actual_duration =
                    subtitle_stream.actual_duration_seconds(metadata.duration_seconds());
                println!("\tDuration: {:.2} seconds", actual_duration);
                println!("\tDisposition: {:?}", subtitle_stream.disposition);
                println!("\tDiscard: {:?}", subtitle_stream.discard);

                // Print stream metadata
                if !subtitle_stream.metadata.is_empty() {
                    println!("\tStream Metadata:");
                    for (k, v) in &subtitle_stream.metadata {
                        println!("\t\t{}: {}", k, v);
                    }
                }
            }
        }
    }

    Ok(())
}
