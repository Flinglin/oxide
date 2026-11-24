impl SftpSession {
    pub async fn preview(&self, path: &str) -> Result<PreviewContent, SftpError> {
        self.preview_with_offset(path, 0).await
    }

    pub async fn preview_with_offset(
        &self,
        path: &str,
        offset: u64,
    ) -> Result<PreviewContent, SftpError> {
        let audit = self.audit_operation("file_preview", path);
        let audit_result = async {
            let canonical_path = self.resolve_path(path).await?;
            let metadata = self
                .sftp
                .metadata(&canonical_path)
                .await
                .map_err(|error| self.map_sftp_error(error, &canonical_path))?;
            let file_size = metadata.size.unwrap_or(0);
            let file_name = Path::new(&canonical_path)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            let extension = Path::new(&canonical_path)
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            let mime_type = mime_guess::from_path(&canonical_path)
                .first_or_octet_stream()
                .to_string();
            let header = zeroize::Zeroizing::new(
                self.read_file_limited(&canonical_path, file_size.min(64) as usize)
                    .await?,
            );
            if let Some(mime) = oxide_preview::inspection_mime_type(&extension, &header) {
                return self
                    .preview_asset(&canonical_path, file_size, mime, AssetFileKind::Document)
                    .await;
            }

            let sqlite = matches!(extension.as_str(), "sqlite" | "sqlite3")
                || (matches!(extension.as_str(), "db" | "db3")
                    && self.read_file_limited(&canonical_path, 16).await? == b"SQLite format 3\0");
            if sqlite {
                if file_size > constants::MAX_PREVIEW_SIZE {
                    return Ok(PreviewContent::TooLarge {
                        size: file_size,
                        max_size: constants::MAX_PREVIEW_SIZE,
                        recommend_download: true,
                    });
                }
                if !self.sqlite_snapshot_ready(&canonical_path).await? {
                    return Ok(sqlite_snapshot_unavailable());
                }
                let downloaded = self
                    .download_to_temp(&canonical_path, constants::MAX_PREVIEW_SIZE)
                    .await?;
                // Restore RAII ownership during the post-download checks and cancellation boundary.
                let temp =
                    tempfile::TempPath::try_from_path(downloaded).map_err(SftpError::IoError)?;
                let after = self
                    .sftp
                    .metadata(&canonical_path)
                    .await
                    .map_err(|error| self.map_sftp_error(error, &canonical_path))?;
                if !self.sqlite_snapshot_ready(&canonical_path).await?
                    || after.size != metadata.size
                    || after.mtime != metadata.mtime
                {
                    return Ok(sqlite_snapshot_unavailable());
                }
                let path = temp
                    .keep()
                    .map_err(|error| SftpError::IoError(error.error))?;
                return Ok(PreviewContent::AssetFile {
                    path: path.to_string_lossy().into(),
                    mime_type: "application/vnd.sqlite3".into(),
                    kind: AssetFileKind::Document,
                });
            }

            if is_text_extension(&extension) {
                return self
                    .preview_text(&canonical_path, &extension, &mime_type, file_size)
                    .await;
            }
            if file_name.starts_with('.') && extension.is_empty() {
                return self
                    .preview_text(&canonical_path, "conf", &mime_type, file_size)
                    .await;
            }
            if extension == "pdf" || mime_type == "application/pdf" {
                return self
                    .preview_asset(
                        &canonical_path,
                        file_size,
                        &mime_type,
                        AssetFileKind::Document,
                    )
                    .await;
            }
            if is_office_extension(&extension) {
                return self
                    .preview_asset(
                        &canonical_path,
                        file_size,
                        &mime_type,
                        AssetFileKind::Office,
                    )
                    .await;
            }
            if is_font_extension(&extension) || mime_type.starts_with("font/") {
                let font_mime_type = font_mime_type(&extension, &mime_type);
                return self
                    .preview_asset(
                        &canonical_path,
                        file_size,
                        &font_mime_type,
                        AssetFileKind::Font,
                    )
                    .await;
            }
            if mime_type.starts_with("image/") {
                return self
                    .preview_image(&canonical_path, file_size, &mime_type)
                    .await;
            }
            if mime_type.starts_with("video/")
                || matches!(
                    extension.as_str(),
                    "mp4" | "webm" | "ogg" | "mov" | "mkv" | "avi"
                )
            {
                return self
                    .preview_asset(&canonical_path, file_size, &mime_type, AssetFileKind::Video)
                    .await;
            }
            if mime_type.starts_with("audio/")
                || matches!(
                    extension.as_str(),
                    "mp3" | "wav" | "ogg" | "flac" | "aac" | "m4a"
                )
            {
                return self
                    .preview_asset(&canonical_path, file_size, &mime_type, AssetFileKind::Audio)
                    .await;
            }

            let is_text_mime = mime_type.starts_with("text/")
                || matches!(
                    mime_type.as_str(),
                    "application/json"
                        | "application/xml"
                        | "application/javascript"
                        | "application/toml"
                        | "application/yaml"
                );
            if is_text_mime {
                return self
                    .preview_text(&canonical_path, &extension, &mime_type, file_size)
                    .await;
            }
            if (extension.is_empty() || mime_type == "application/octet-stream")
                && file_size <= constants::MAX_TEXT_PREVIEW_SIZE
            {
                let sample = self
                    .read_file_limited(&canonical_path, file_size.min(8192) as usize)
                    .await?;
                if is_likely_text_content(&sample) {
                    return self
                        .preview_text(&canonical_path, "txt", "text/plain", file_size)
                        .await;
                }
            }

            self.preview_hex(&canonical_path, file_size, offset).await
        }
        .await;
        audit.result(&audit_result);
        audit_result
    }
}

fn sqlite_snapshot_unavailable() -> PreviewContent {
    PreviewContent::Unsupported {
        mime_type: "application/vnd.sqlite3".into(),
        reason: "file_preview.sqlite_snapshot_required".into(),
    }
}

impl SftpSession {
    async fn sqlite_snapshot_ready(&self, path: &str) -> Result<bool, SftpError> {
        for suffix in ["-wal", "-journal"] {
            let sidecar = format!("{path}{suffix}");
            match self.sftp.metadata(&sidecar).await {
                Ok(metadata) if metadata.size == Some(0) => {}
                Ok(_) => return Ok(false),
                Err(error) => match self.map_sftp_error(error, &sidecar) {
                    SftpError::FileNotFound(_) => {}
                    error => return Err(error),
                },
            }
        }
        Ok(true)
    }
}
