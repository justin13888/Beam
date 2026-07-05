//! Example code to generate a HLS media playlist (to a hypothetical server) from a video file

use beam_server::services::{hash::LocalHashService, media_info::LocalMediaInfoService};
use beam_server::utils::file::FileType;
use beam_server::utils::metadata::VideoFileMetadata;
use beam_server::utils::stream::StreamBuilder;
use beam_server::utils::stream::hls::HlsStreamGenerator;
use eyre::Result;
use m3u8_rs::WRITE_OPT_FLOAT_PRECISION;
use std::{path::PathBuf, sync::Arc, sync::atomic::Ordering, time::Instant};

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;
    dotenvy::dotenv().ok();
    beam_server::logging::init_tracing();

    // Get first argument as file path
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <video_file>", args[0]);
        std::process::exit(1);
    }
    let file_path = PathBuf::from(&args[1]);

    let start_time = Instant::now();

    // Initialize FFmpeg
    ffmpeg_next::init().unwrap();

    // Generate metadata for video file
    let metadata = VideoFileMetadata::from_path(&file_path).expect("Failed to extract metadata");
    println!("Metadata: {:#?}", metadata);

    WRITE_OPT_FLOAT_PRECISION.store(5, Ordering::Relaxed);

    // === Generate HLS playlist in directory ===
    let hash_service = Arc::new(LocalHashService::default());
    let media_info_service = Arc::new(LocalMediaInfoService::default());
    let mut stream_builder = StreamBuilder::new(hash_service, media_info_service);
    stream_builder.add_file(FileType::Video, &file_path);
    let stream_configuration = stream_builder.build().await?;
    println!("Stream configuration: {:#?}", stream_configuration);
    let hls_generator = HlsStreamGenerator::from(stream_configuration);
    let master_playlist = hls_generator.get_master_playlist();
    let playlists = hls_generator.get_media_playlists();

    // === Print out the generated playlists ===

    let mut master_playlist_v: Vec<u8> = Vec::new();
    master_playlist.write_to(&mut master_playlist_v).unwrap();
    let master_playlist_str: &str = std::str::from_utf8(&master_playlist_v).unwrap();
    println!("Master playlist...\n{master_playlist_str}\n");

    let stream_playlists: Vec<_> = playlists
        .into_iter()
        .map(|(playlist_uri, playlist)| {
            let mut v: Vec<u8> = Vec::new();
            playlist.write_to(&mut v).unwrap();
            (playlist_uri, std::str::from_utf8(&v).unwrap().to_string())
        })
        .collect();
    println!("Stream playlists...");
    for (uri, pl) in stream_playlists.iter() {
        println!("{uri}:\n{pl}\n");
    }

    println!("Total time: {:.2?}", start_time.elapsed());

    Ok(())
}
