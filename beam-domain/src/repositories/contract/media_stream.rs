/// Behavioural contract for [`crate::repositories::MediaStreamRepository`]
/// (issue #349): a file's streams read back in index order with every field
/// of their metadata, whatever order they were inserted in; an insert reports
/// how many it wrote; files are kept apart; and deleting a file's streams
/// takes exactly those, reporting how many.
///
/// `$setup` names an `async fn() -> impl MediaStreamFixture`.
#[macro_export]
macro_rules! media_stream_repository_contract {
    ($setup:path) => {
        use ::uuid::Uuid;
        use $crate::models::stream::{
            AudioStreamMetadata, CreateMediaStream, MediaStream, StreamMetadata, StreamType,
            SubtitleStreamMetadata, VideoStreamMetadata,
        };
        use $crate::repositories::contract::fixture::MediaStreamFixture;

        fn video(file_id: Uuid, index: u32) -> CreateMediaStream {
            CreateMediaStream {
                file_id,
                index,
                stream_type: StreamType::Video,
                codec: "hevc".to_string(),
                metadata: StreamMetadata::Video(VideoStreamMetadata {
                    width: 3840,
                    height: 2160,
                    frame_rate: Some(23.976),
                    bit_rate: Some(42_000_000),
                    color_space: Some("bt2020nc".to_string()),
                    color_range: Some("tv".to_string()),
                    hdr_format: Some("hdr10".to_string()),
                }),
            }
        }

        fn audio(file_id: Uuid, index: u32) -> CreateMediaStream {
            CreateMediaStream {
                file_id,
                index,
                stream_type: StreamType::Audio,
                codec: "eac3".to_string(),
                metadata: StreamMetadata::Audio(AudioStreamMetadata {
                    language: Some("jpn".to_string()),
                    title: Some("Original 5.1".to_string()),
                    channels: 6,
                    sample_rate: 48_000,
                    channel_layout: Some("5.1(side)".to_string()),
                    bit_rate: Some(640_000),
                    is_default: true,
                    is_forced: false,
                }),
            }
        }

        fn subtitle(file_id: Uuid, index: u32) -> CreateMediaStream {
            CreateMediaStream {
                file_id,
                index,
                stream_type: StreamType::Subtitle,
                codec: "subrip".to_string(),
                metadata: StreamMetadata::Subtitle(SubtitleStreamMetadata {
                    language: Some("eng".to_string()),
                    title: Some("SDH".to_string()),
                    is_default: false,
                    is_forced: true,
                    is_hearing_impaired: true,
                }),
            }
        }

        /// Every field but the store-assigned id: the metadata types carry
        /// no `PartialEq`, so they are compared through their full `Debug`
        /// rendering, which names every field.
        fn written(stream: &CreateMediaStream) -> String {
            let CreateMediaStream {
                file_id,
                index,
                stream_type,
                codec,
                metadata,
            } = stream;
            format!("{file_id} {index} {stream_type:?} {codec} {metadata:?}")
        }

        fn read(stream: &MediaStream) -> String {
            let MediaStream {
                id: _,
                file_id,
                index,
                stream_type,
                codec,
                metadata,
            } = stream;
            format!("{file_id} {index} {stream_type:?} {codec} {metadata:?}")
        }

        #[tokio::test]
        async fn a_files_streams_read_back_by_index_with_every_field() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            let streams = vec![subtitle(file, 2), video(file, 0), audio(file, 1)];

            assert_eq!(repo.insert_streams(streams).await.unwrap(), 3);

            let found = repo.find_by_file_id(file).await.unwrap();
            assert_eq!(
                found.iter().map(read).collect::<Vec<_>>(),
                [video(file, 0), audio(file, 1), subtitle(file, 2)]
                    .iter()
                    .map(written)
                    .collect::<Vec<_>>()
            );
            let mut ids: Vec<Uuid> = found.iter().map(|stream| stream.id).collect();
            ids.sort();
            ids.dedup();
            assert_eq!(ids.len(), 3, "every stream has an id of its own");
        }

        #[tokio::test]
        async fn a_later_insert_adds_to_a_files_streams_and_an_empty_one_writes_none() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;

            assert_eq!(repo.insert_streams(Vec::new()).await.unwrap(), 0);
            assert!(repo.find_by_file_id(file).await.unwrap().is_empty());

            repo.insert_streams(vec![audio(file, 1)]).await.unwrap();
            repo.insert_streams(vec![video(file, 0)]).await.unwrap();
            let indexes: Vec<u32> = repo
                .find_by_file_id(file)
                .await
                .unwrap()
                .iter()
                .map(|stream| stream.index)
                .collect();
            assert_eq!(indexes, [0, 1]);
        }

        #[tokio::test]
        async fn deleting_a_files_streams_takes_only_those() {
            let fixture = $setup().await;
            let repo = fixture.repo();
            let file = fixture.new_file().await;
            let other = fixture.new_file().await;
            repo.insert_streams(vec![video(file, 0), audio(file, 1), subtitle(file, 2)])
                .await
                .unwrap();
            repo.insert_streams(vec![video(other, 0)]).await.unwrap();

            assert_eq!(repo.delete_by_file_id(file).await.unwrap(), 3);
            assert!(repo.find_by_file_id(file).await.unwrap().is_empty());
            assert_eq!(
                repo.find_by_file_id(other)
                    .await
                    .unwrap()
                    .iter()
                    .map(read)
                    .collect::<Vec<_>>(),
                [written(&video(other, 0))]
            );
            assert_eq!(repo.delete_by_file_id(file).await.unwrap(), 0);
            assert_eq!(repo.delete_by_file_id(Uuid::new_v4()).await.unwrap(), 0);
            assert!(
                repo.find_by_file_id(Uuid::new_v4())
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
    };
}
