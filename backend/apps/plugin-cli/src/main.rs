//! 插件打包命令行，校验输入资源并生成带摘要的平台归档

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read as _, Write as _},
    path::{Component, Path, PathBuf},
};

use clap::{Args, Parser, Subcommand};
use flate2::{Compression, GzBuilder};
use gateway_plugin_sdk::{
    Manifest, ManifestError, PROTOCOL_VERSION, Package, PackageTarget, valid_package_path,
};
use sha2::{Digest as _, Sha256};
use tar::{Builder, EntryType, Header, HeaderMode};
use thiserror::Error;

const MAXIMUM_COMPRESSED_BYTES: u64 = 32 * 1024 * 1024;
const MAXIMUM_EXPANDED_BYTES: u64 = 128 * 1024 * 1024;
const MAXIMUM_MANIFEST_BYTES: u64 = 64 * 1024;

#[derive(Parser)]
#[command(name = "cpr-plugin", version, about = "Codex Proxy 插件开发工具")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    #[command(about = "校验清单并打包已编译的插件")]
    Package(Options),
}

#[derive(Debug, Error)]
enum PackagerError {
    #[error("{0}")]
    Invalid(String),
    #[error("failed to {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("source or generated manifest is invalid: {0}")]
    Manifest(#[from] ManifestError),
    #[error("failed to encode generated manifest: {0}")]
    EncodeManifest(#[source] serde_json::Error),
}

#[derive(Debug, Args)]
struct Options {
    #[arg(long, value_name = "FILE", help = "作者清单 plugin.json")]
    manifest: PathBuf,
    #[arg(long, value_name = "FILE", help = "已编译的目标平台插件二进制")]
    binary: PathBuf,
    #[arg(long, value_name = "TRIPLE", help = "目标平台 triple")]
    target: String,
    #[arg(long, value_name = "DIRECTORY", help = "归档与 SHA-256 文件的输出目录")]
    output_dir: PathBuf,
    #[arg(
        long = "resource-map",
        value_name = "PREFIX=DIRECTORY",
        help = "包内资源前缀到项目内目录的映射，可重复传入"
    )]
    resource_maps: Vec<String>,
}

#[derive(Debug, Clone, Copy)]
struct RuntimeTarget {
    os: &'static str,
    architecture: &'static str,
}

impl RuntimeTarget {
    fn parse(triple: &str) -> Result<Self, PackagerError> {
        match triple {
            "x86_64-unknown-linux-gnu" => Ok(Self {
                os: "linux",
                architecture: "x86_64",
            }),
            "aarch64-unknown-linux-gnu" => Ok(Self {
                os: "linux",
                architecture: "aarch64",
            }),
            "aarch64-apple-darwin" => Ok(Self {
                os: "macos",
                architecture: "aarch64",
            }),
            _ => Err(PackagerError::Invalid(format!(
                "unsupported plugin target {triple}"
            ))),
        }
    }
}

#[derive(Debug)]
struct PackageOutput {
    archive: PathBuf,
    checksum: PathBuf,
}

fn main() -> Result<(), PackagerError> {
    match Cli::parse().command {
        Commands::Package(options) => {
            let output = package(&options)?;
            println!("{}", output.archive.display());
            println!("{}", output.checksum.display());
            Ok(())
        }
    }
}

fn parse_resource_maps(values: &[String]) -> Result<BTreeMap<String, PathBuf>, PackagerError> {
    let mut resource_maps = BTreeMap::new();
    for value in values {
        let (prefix, directory) = value.split_once('=').ok_or_else(|| {
            PackagerError::Invalid(
                "--resource-map must use package-prefix=source-directory".to_owned(),
            )
        })?;
        if !valid_resource_prefix(prefix) {
            return Err(PackagerError::Invalid(format!(
                "invalid resource-map package prefix {prefix}"
            )));
        }
        let directory = PathBuf::from(directory);
        if !safe_relative_directory(&directory) {
            return Err(PackagerError::Invalid(format!(
                "resource-map source directory must be a non-empty project-relative path: {}",
                directory.display()
            )));
        }
        if resource_maps.insert(prefix.to_owned(), directory).is_some() {
            return Err(PackagerError::Invalid(format!(
                "duplicate resource-map package prefix {prefix}"
            )));
        }
    }
    Ok(resource_maps)
}

fn valid_resource_prefix(prefix: &str) -> bool {
    valid_package_path(prefix) && !prefix.contains('/')
}

fn safe_relative_directory(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

fn package(options: &Options) -> Result<PackageOutput, PackagerError> {
    let target = RuntimeTarget::parse(&options.target)?;
    let resource_maps = parse_resource_maps(&options.resource_maps)?;
    let manifest_bytes = read_regular_file(&options.manifest, MAXIMUM_MANIFEST_BYTES)?;
    let mut manifest = Manifest::from_author_slice(&manifest_bytes)?;

    let project_directory = options
        .manifest
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let canonical_project = canonicalize(project_directory)?;
    let mut sources = BTreeMap::from([(manifest.main.clone(), options.binary.clone())]);
    let mut used_maps = BTreeSet::new();
    for package_path in manifest.resources.keys() {
        let source = resource_source(
            project_directory,
            package_path,
            &resource_maps,
            &mut used_maps,
        )?;
        let canonical_source = canonicalize(&source)?;
        if !canonical_source.starts_with(&canonical_project) {
            return Err(PackagerError::Invalid(format!(
                "resource source escapes the plugin project: {}",
                source.display()
            )));
        }
        if sources.insert(package_path.clone(), source).is_some() {
            return Err(PackagerError::Invalid(format!(
                "plugin main and resource share package path {package_path}"
            )));
        }
    }
    if used_maps.len() != resource_maps.len() {
        let unused = resource_maps
            .keys()
            .filter(|prefix| !used_maps.contains(*prefix))
            .cloned()
            .collect::<Vec<_>>();
        return Err(PackagerError::Invalid(format!(
            "unused resource-map package prefix: {}",
            unused.join(", ")
        )));
    }
    reject_case_folded_paths(sources.keys())?;

    let mut files = BTreeMap::new();
    let mut source_bytes = 0u64;
    for (package_path, source) in sources {
        let remaining = MAXIMUM_EXPANDED_BYTES
            .checked_sub(source_bytes)
            .ok_or_else(package_limit)?;
        let bytes = read_regular_file(&source, remaining)?;
        source_bytes = source_bytes
            .checked_add(u64::try_from(bytes.len()).map_err(|_| package_limit())?)
            .ok_or_else(package_limit)?;
        files.insert(package_path, bytes);
    }
    let digests = files
        .iter()
        .map(|(path, bytes)| (path.clone(), hex::encode(Sha256::digest(bytes))))
        .collect();
    manifest.package = Some(Package {
        protocol_version: PROTOCOL_VERSION,
        target: PackageTarget {
            os: target.os.to_owned(),
            architecture: target.architecture.to_owned(),
        },
        files: digests,
    });
    manifest.validate()?;
    let mut generated_manifest =
        serde_json::to_vec_pretty(&manifest).map_err(PackagerError::EncodeManifest)?;
    generated_manifest.push(b'\n');
    if generated_manifest.len() as u64 > MAXIMUM_MANIFEST_BYTES {
        return Err(package_limit());
    }

    create_directory(&options.output_dir)?;
    let plugin_id = manifest.plugin_id()?;
    let archive_name = format!("{plugin_id}-{}-{}.tar.gz", manifest.version, options.target);
    let archive_path = options.output_dir.join(&archive_name);
    let archive_temporary = options
        .output_dir
        .join(format!(".{archive_name}.{}.tmp", std::process::id()));
    let mut temporary = TemporaryFile::create(archive_temporary)?;
    write_archive(
        temporary.file_mut(),
        &generated_manifest,
        &manifest.main,
        &files,
    )?;
    temporary.sync_all()?;
    let compressed_bytes = temporary.metadata_len()?;
    if compressed_bytes > MAXIMUM_COMPRESSED_BYTES {
        return Err(package_limit());
    }
    temporary.persist(&archive_path)?;

    let archive_digest = sha256_file(&archive_path)?;
    let checksum_path = options.output_dir.join(format!("{archive_name}.sha256"));
    let checksum_temporary = options
        .output_dir
        .join(format!(".{archive_name}.sha256.{}.tmp", std::process::id()));
    let mut checksum = TemporaryFile::create(checksum_temporary)?;
    let checksum_temporary_path = checksum.path().to_owned();
    writeln!(checksum.file_mut(), "{archive_digest}  {archive_name}").map_err(|source| {
        PackagerError::Io {
            action: "write",
            path: checksum_temporary_path,
            source,
        }
    })?;
    checksum.sync_all()?;
    checksum.persist(&checksum_path)?;

    Ok(PackageOutput {
        archive: archive_path,
        checksum: checksum_path,
    })
}

fn resource_source(
    project_directory: &Path,
    package_path: &str,
    mappings: &BTreeMap<String, PathBuf>,
    used_mappings: &mut BTreeSet<String>,
) -> Result<PathBuf, PackagerError> {
    let (prefix, remainder) = package_path
        .split_once('/')
        .map_or((package_path, None), |(prefix, remainder)| {
            (prefix, Some(remainder))
        });
    if let Some(directory) = mappings.get(prefix) {
        let remainder = remainder.ok_or_else(|| {
            PackagerError::Invalid(format!(
                "mapped resource {package_path} must have a path below prefix {prefix}"
            ))
        })?;
        used_mappings.insert(prefix.to_owned());
        Ok(project_directory.join(directory).join(remainder))
    } else {
        Ok(project_directory.join(package_path))
    }
}

fn reject_case_folded_paths<'a>(
    paths: impl IntoIterator<Item = &'a String>,
) -> Result<(), PackagerError> {
    let mut folded = BTreeSet::new();
    for path in paths {
        if !folded.insert(path.to_ascii_lowercase()) {
            return Err(PackagerError::Invalid(format!(
                "package path collides case-insensitively: {path}"
            )));
        }
    }
    Ok(())
}

fn write_archive(
    output: &mut File,
    manifest: &[u8],
    main_path: &str,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), PackagerError> {
    let encoder = GzBuilder::new().mtime(0).write(output, Compression::best());
    let limited = LimitedWriter::new(encoder, MAXIMUM_EXPANDED_BYTES);
    let mut archive = Builder::new(limited);
    archive.mode(HeaderMode::Deterministic);
    append_file(&mut archive, "plugin.json", manifest, 0o644)?;
    for (path, bytes) in files {
        let mode = if path == main_path { 0o755 } else { 0o644 };
        append_file(&mut archive, path, bytes, mode)?;
    }
    archive.finish().map_err(|source| PackagerError::Io {
        action: "finish archive",
        path: PathBuf::from("<temporary archive>"),
        source,
    })?;
    let limited = archive.into_inner().map_err(|source| PackagerError::Io {
        action: "finish archive",
        path: PathBuf::from("<temporary archive>"),
        source,
    })?;
    limited
        .into_inner()
        .finish()
        .map_err(|source| PackagerError::Io {
            action: "finish compression",
            path: PathBuf::from("<temporary archive>"),
            source,
        })?;
    Ok(())
}

fn append_file<W: io::Write>(
    archive: &mut Builder<W>,
    path: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<(), PackagerError> {
    let mut header = Header::new_gnu();
    header.set_entry_type(EntryType::Regular);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_size(bytes.len() as u64);
    header.set_cksum();
    archive
        .append_data(&mut header, path, bytes)
        .map_err(|source| PackagerError::Io {
            action: "append archive entry",
            path: PathBuf::from(path),
            source,
        })
}

struct LimitedWriter<W> {
    inner: W,
    written: u64,
    maximum: u64,
}

impl<W> LimitedWriter<W> {
    const fn new(inner: W, maximum: u64) -> Self {
        Self {
            inner,
            written: 0,
            maximum,
        }
    }

    fn into_inner(self) -> W {
        self.inner
    }
}

impl<W> io::Write for LimitedWriter<W>
where
    W: io::Write,
{
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = u64::try_from(bytes.len())
            .map_err(|_| io::Error::other("plugin package exceeds expanded size limit"))?;
        if length > self.maximum.saturating_sub(self.written) {
            return Err(io::Error::other(
                "plugin package exceeds expanded size limit",
            ));
        }
        let written = self.inner.write(bytes)?;
        self.written = self
            .written
            .checked_add(written as u64)
            .ok_or_else(|| io::Error::other("plugin package size overflow"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

struct TemporaryFile {
    path: PathBuf,
    file: File,
}

impl TemporaryFile {
    fn create(path: PathBuf) -> Result<Self, PackagerError> {
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|source| PackagerError::Io {
                action: "create",
                path: path.clone(),
                source,
            })?;
        Ok(Self { path, file })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }

    fn sync_all(&mut self) -> Result<(), PackagerError> {
        let path = self.path.clone();
        self.file_mut()
            .sync_all()
            .map_err(|source| PackagerError::Io {
                action: "sync",
                path,
                source,
            })
    }

    fn metadata_len(&self) -> Result<u64, PackagerError> {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .map_err(|source| PackagerError::Io {
                action: "inspect",
                path: self.path.clone(),
                source,
            })
    }

    fn persist(mut self, destination: &Path) -> Result<(), PackagerError> {
        fs::rename(&self.path, destination).map_err(|source| PackagerError::Io {
            action: "publish",
            path: destination.to_owned(),
            source,
        })?;
        self.path.clear();
        Ok(())
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn read_regular_file(path: &Path, maximum_bytes: u64) -> Result<Vec<u8>, PackagerError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| PackagerError::Io {
        action: "inspect",
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_file() || metadata.file_type().is_symlink() {
        return Err(PackagerError::Invalid(format!(
            "package input must be a regular non-symlink file: {}",
            path.display()
        )));
    }
    if metadata.len() > maximum_bytes {
        return Err(package_limit());
    }
    let file = File::open(path).map_err(|source| PackagerError::Io {
        action: "open",
        path: path.to_owned(),
        source,
    })?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or_default());
    file.take(maximum_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| PackagerError::Io {
            action: "read",
            path: path.to_owned(),
            source,
        })?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(package_limit());
    }
    Ok(bytes)
}

fn canonicalize(path: &Path) -> Result<PathBuf, PackagerError> {
    fs::canonicalize(path).map_err(|source| PackagerError::Io {
        action: "canonicalize",
        path: path.to_owned(),
        source,
    })
}

fn create_directory(path: &Path) -> Result<(), PackagerError> {
    fs::create_dir_all(path).map_err(|source| PackagerError::Io {
        action: "create directory",
        path: path.to_owned(),
        source,
    })
}

fn sha256_file(path: &Path) -> Result<String, PackagerError> {
    let mut file = File::open(path).map_err(|source| PackagerError::Io {
        action: "open",
        path: path.to_owned(),
        source,
    })?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|source| PackagerError::Io {
            action: "hash",
            path: path.to_owned(),
            source,
        })?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex::encode(digest.finalize()))
}

fn package_limit() -> PackagerError {
    PackagerError::Invalid("plugin package exceeds installation size limits".to_owned())
}
