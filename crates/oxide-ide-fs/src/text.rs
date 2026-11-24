use encoding_rs::{Encoding, UTF_8, UTF_16BE, UTF_16LE};
use oxide_ide_core::{
    IdeFileData, IdeFileError, IdeFileErrorKind, LineEnding, MAX_EDITABLE_FILE_SIZE,
    SavedFileVersion, TextFileFormat,
};
use oxide_preview::{
    TextLineEnding, detect_and_decode_with_hint, is_likely_text_content,
    normalize_text_line_endings, restore_text_line_endings,
};
use zeroize::Zeroizing;
pub(crate) const FILE_PREFIX_BYTES: usize = 8192;

pub(crate) fn check_text_prefix(bytes: &[u8], hint: Option<&str>) -> Result<(), IdeFileError> {
    let explicit_unicode = hint
        .and_then(|h| Encoding::for_label(h.as_bytes()))
        .is_some_and(|e| e == UTF_16LE || e == UTF_16BE);
    if explicit_unicode
        || bytes.starts_with(&[0xff, 0xfe])
        || bytes.starts_with(&[0xfe, 0xff])
        || is_likely_text_content(bytes)
    {
        Ok(())
    } else {
        Err(IdeFileError::new(
            IdeFileErrorKind::Binary,
            "File is not text",
        ))
    }
}
pub(crate) fn check_size(size: u64) -> Result<(), IdeFileError> {
    if size > MAX_EDITABLE_FILE_SIZE {
        return Err(IdeFileError::new(
            IdeFileErrorKind::TooLarge,
            "File exceeds the 100 MiB IDE limit",
        ));
    }
    Ok(())
}

pub(crate) fn decode_file(
    bytes: &[u8],
    hint: Option<&str>,
    version: SavedFileVersion,
) -> Result<IdeFileData, IdeFileError> {
    check_size(bytes.len() as u64)?;
    check_text_prefix(bytes, hint)?;
    // A manual reopen must honor the selected encoding, even when a conflicting BOM exists.
    // Automatic detection remains shared with previews; editing rejects lossy decoding.
    let (decoded, name, has_bom, errors) = if let Some(hint) = hint {
        let encoding = Encoding::for_label(hint.as_bytes()).ok_or_else(invalid_encoding)?;
        let bom_len = Encoding::for_bom(bytes)
            .filter(|(bom_encoding, _)| *bom_encoding == encoding)
            .map(|(_, len)| len)
            .unwrap_or(0);
        let (text, errors) = encoding.decode_without_bom_handling(&bytes[bom_len..]);
        (
            text.into_owned(),
            encoding.name().to_string(),
            bom_len > 0,
            errors,
        )
    } else {
        let (text, name, _, bom, errors) = detect_and_decode_with_hint(bytes, None);
        (text, name, bom, errors)
    };
    let decoded = Zeroizing::new(decoded);
    if errors {
        return Err(invalid_encoding());
    }
    let (text, line_ending) = normalize_text_line_endings(&decoded);
    let line_ending = match line_ending {
        TextLineEnding::Lf => LineEnding::Lf,
        TextLineEnding::CrLf => LineEnding::CrLf,
        TextLineEnding::Cr => LineEnding::Cr,
    };
    Ok(IdeFileData {
        text,
        version,
        format: TextFileFormat {
            encoding: name,
            has_bom,
            line_ending,
        },
    })
}

pub(crate) fn encode_file(
    text: &str,
    format: &TextFileFormat,
) -> Result<Zeroizing<Vec<u8>>, IdeFileError> {
    let encoding = Encoding::for_label(format.encoding.as_bytes()).ok_or_else(invalid_encoding)?;
    let ending = match format.line_ending {
        LineEnding::Lf => TextLineEnding::Lf,
        LineEnding::CrLf => TextLineEnding::CrLf,
        LineEnding::Cr => TextLineEnding::Cr,
    };
    let text = Zeroizing::new(restore_text_line_endings(text, ending));
    let mut bytes = Zeroizing::new(Vec::new());
    if encoding == UTF_16LE || encoding == UTF_16BE {
        if format.has_bom {
            bytes.extend_from_slice(if encoding == UTF_16LE {
                &[0xff, 0xfe]
            } else {
                &[0xfe, 0xff]
            });
        }
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&if encoding == UTF_16LE {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
    } else {
        if format.has_bom && encoding == UTF_8 {
            bytes.extend_from_slice(&[0xef, 0xbb, 0xbf]);
        }
        let (encoded, _, errors) = encoding.encode(&text);
        let encoded = Zeroizing::new(encoded.into_owned());
        if errors {
            return Err(IdeFileError::new(
                IdeFileErrorKind::UnrepresentableText,
                "The selected encoding cannot represent this text",
            ));
        }
        bytes.extend_from_slice(&encoded);
    }
    check_size(bytes.len() as u64)?;
    Ok(bytes)
}

fn invalid_encoding() -> IdeFileError {
    IdeFileError::new(
        IdeFileErrorKind::InvalidEncoding,
        "Cannot decode using this encoding",
    )
}
