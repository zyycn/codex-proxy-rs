use provider_openai::transport::profile::platform_release::xz::IndexedXz;
use std::io::{Cursor, Read, Seek, SeekFrom, Write};

pub(super) fn compress(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = xz2::write::XzEncoder::new(Vec::new(), 1);
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}
#[test]
fn indexed_xz_supports_backward_and_end_relative_seeks() {
    let bytes: Vec<u8> = (0..100_000).map(|i| (i % 251) as u8).collect();
    let compressed = compress(&bytes);
    let mut reader = IndexedXz::new(Cursor::new(&compressed), 0, compressed.len() as u64).unwrap();
    for (seek, offset) in [
        (SeekFrom::Start(80000), 80000),
        (SeekFrom::Start(12), 12),
        (SeekFrom::End(-50), 99950),
    ] {
        reader.seek(seek).unwrap();
        let mut actual = [0; 50];
        reader.read_exact(&mut actual).unwrap();
        assert_eq!(&actual, &bytes[offset..offset + 50]);
    }
    assert!(reader.seek(SeekFrom::End(1)).is_err());
}

#[test]
fn indexed_xz_reads_across_blocks_and_can_skip_a_corrupt_unused_block() {
    use xz2::stream::{Action, Check, Status, Stream};
    let bytes: Vec<u8> = (0..100_000).map(|i| (i % 251) as u8).collect();
    let mut stream = Stream::new_easy_encoder(1, Check::Crc64).unwrap();
    let mut compressed = Vec::with_capacity(200_000);
    assert_eq!(
        stream
            .process_vec(&bytes[..50_000], &mut compressed, Action::FullFlush)
            .unwrap(),
        Status::StreamEnd
    );
    let first_block_end = compressed.len();
    assert_eq!(
        stream
            .process_vec(&bytes[50_000..], &mut compressed, Action::Finish)
            .unwrap(),
        Status::StreamEnd
    );
    let mut reader = IndexedXz::new(Cursor::new(&compressed), 0, compressed.len() as u64).unwrap();
    reader.seek(SeekFrom::Start(49_990)).unwrap();
    let mut actual = [0; 40];
    reader.read_exact(&mut actual).unwrap();
    assert_eq!(actual, bytes[49_990..50_030]);
    // 未访问的块不会下载、解压；访问它时仍须拒绝校验和错误。
    compressed[first_block_end - 1] ^= 1;
    let mut reader = IndexedXz::new(Cursor::new(&compressed), 0, compressed.len() as u64).unwrap();
    reader.seek(SeekFrom::Start(80_000)).unwrap();
    reader.read_exact(&mut actual).unwrap();
    assert_eq!(actual, bytes[80_000..80_040]);
    reader.seek(SeekFrom::Start(0)).unwrap();
    assert!(reader.read_exact(&mut actual).is_err());
}
