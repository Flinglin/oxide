use thiserror::Error;

pub use oxide_environment::archive::{ArchiveKind, archive_kind};

/// A shell command and its recognized archive format.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArchiveExtractionPlan {
    pub kind: ArchiveKind,
    pub command: String,
}

/// Errors produced while planning remote archive extraction.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ArchiveExtractionError {
    #[error("unsupported archive: {file_name}")]
    UnsupportedArchive { file_name: String },
}

/// Builds a non-destructive remote extraction command with shell-quoted paths.
pub fn plan_archive_extraction(
    file_name: &str,
    archive_path: &str,
    destination_path: &str,
) -> Result<ArchiveExtractionPlan, ArchiveExtractionError> {
    let kind =
        archive_kind(file_name).ok_or_else(|| ArchiveExtractionError::UnsupportedArchive {
            file_name: file_name.to_string(),
        })?;
    let archive = shell_quote(archive_path);
    let destination = shell_quote(destination_path);
    // Keep extraction non-destructive until SFTP has an archive conflict dialog.
    let command = match kind {
        ArchiveKind::Zip => format!("unzip -nq {archive} -d {destination}"),
        ArchiveKind::Tar => format!("tar -k -xf {archive} -C {destination}"),
        ArchiveKind::TarGzip => format!("tar -k -xzf {archive} -C {destination}"),
        ArchiveKind::TarBzip2 => format!("tar -k -xjf {archive} -C {destination}"),
        ArchiveKind::TarXz => format!("tar -k -xJf {archive} -C {destination}"),
        ArchiveKind::TarZstd => format!("tar -k --zstd -xf {archive} -C {destination}"),
    };

    Ok(ArchiveExtractionPlan { kind, command })
}

/// Quotes one value for use as data in a POSIX-compatible shell command.
pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
