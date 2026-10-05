//! 验证官方发行目录的有界读取与符号链接拒绝

use std::fs;

use gateway_admin::ports::plugin_release::{
    OfficialPluginReleaseFiles as _, OfficialPluginReleaseReadErrorKind,
};
use gateway_host::official_plugins::FileOfficialPluginRelease;

#[tokio::test]
async fn release_files_are_absent_or_read_from_one_flat_bounded_directory() {
    let temp = tempfile::tempdir().expect("tempdir");
    let directory = temp.path().join("plugins/official");
    let source = FileOfficialPluginRelease::new(directory.clone());
    assert!(source.manifest().await.expect("absent manifest").is_none());

    fs::create_dir_all(&directory).expect("release directory");
    fs::write(directory.join("plugin-release-manifest.json"), b"manifest").expect("manifest");
    fs::write(directory.join("example.tar.gz"), b"archive").expect("archive");
    assert_eq!(
        source.manifest().await.expect("manifest").unwrap().as_ref(),
        b"manifest"
    );
    assert_eq!(
        source
            .artifact("example.tar.gz")
            .await
            .expect("archive")
            .as_ref(),
        b"archive"
    );
    assert_eq!(
        source
            .artifact("../example.tar.gz")
            .await
            .expect_err("path escape")
            .kind(),
        OfficialPluginReleaseReadErrorKind::Invalid
    );
}

#[tokio::test]
async fn release_files_reject_symlinks_and_oversized_manifests() {
    let temp = tempfile::tempdir().expect("tempdir");
    let directory = temp.path().join("official");
    fs::create_dir_all(&directory).expect("release directory");
    let source = FileOfficialPluginRelease::new(directory.clone());
    fs::write(
        directory.join("plugin-release-manifest.json"),
        vec![b'x'; 256 * 1024 + 1],
    )
    .expect("large manifest");
    assert_eq!(
        source
            .manifest()
            .await
            .expect_err("oversized manifest")
            .kind(),
        OfficialPluginReleaseReadErrorKind::Invalid
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let target = temp.path().join("target.tar.gz");
        fs::write(&target, b"archive").expect("target");
        symlink(&target, directory.join("linked.tar.gz")).expect("symlink");
        assert_eq!(
            source
                .artifact("linked.tar.gz")
                .await
                .expect_err("symlink")
                .kind(),
            OfficialPluginReleaseReadErrorKind::Invalid
        );

        let release_target = temp.path().join("release-target");
        fs::create_dir(&release_target).expect("release target");
        fs::write(
            release_target.join("plugin-release-manifest.json"),
            b"manifest",
        )
        .expect("target manifest");
        let release_link = temp.path().join("release-link");
        symlink(&release_target, &release_link).expect("release directory symlink");
        assert_eq!(
            FileOfficialPluginRelease::new(release_link)
                .manifest()
                .await
                .expect_err("release directory symlink")
                .kind(),
            OfficialPluginReleaseReadErrorKind::Invalid
        );
    }
}
