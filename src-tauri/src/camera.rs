use crate::types::LogicalFile;

const PICTURE_EXTENSIONS: &[&str] = &[
    "jpg", "jpeg", "jpe", "heic", "heif", "png", "webp", "gif", "tiff", "tif", "bmp", "dng", "raw",
    "cr2", "cr3", "nef", "arw", "orf", "rw2", "srw", "pef", "raf", "avif",
];

const VIDEO_EXTENSIONS: &[&str] = &[
    "mov", "mp4", "m4v", "3gp", "3g2", "avi", "mkv", "webm", "mts", "m2ts", "mpg", "mpeg", "wmv",
    "flv", "vob", "ogv", "divx",
];

pub fn media_kind(name: &str) -> Option<&'static str> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    if PICTURE_EXTENSIONS.contains(&ext.as_str()) {
        Some("picture")
    } else if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        Some("video")
    } else {
        None
    }
}

/// Names that are clearly not camera captures: screenshots, messaging apps,
/// social networks, screen recordings, wallpapers and downloaded artwork.
pub fn is_obvious_non_camera(name: &str) -> bool {
    let lower = name.to_lowercase();
    const PREFIXES: &[&str] = &[
        "screenshot",
        "screen shot",
        "captura de pantalla",
        "whatsapp",
        "telegram",
        "received_",
        "fb_",
        "instagram",
        "snapchat",
        "screenrecord",
        "rpreplay",
        "vlcsnap",
        "wallpaper",
        "temp",
        "welcome",
        "your.eyes",
        "[lostyears]",
        "@",
    ];
    if PREFIXES.iter().any(|prefix| lower.starts_with(prefix)) {
        return true;
    }
    if let Some(rest) = lower.strip_prefix("wp") {
        if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    const CONTAINS: &[&str] = &[
        "whatsapp",
        "screenshot",
        "screen shot",
        "vlcsnap",
        "http",
        "www.",
        "img-",
    ];
    CONTAINS.iter().any(|needle| lower.contains(needle))
}

pub fn is_camera_capture(file: &LogicalFile) -> bool {
    media_kind(&file.name).is_some() && !is_obvious_non_camera(&file.name)
}

/// Year shown in the virtual camera folder. Uses the date the CLI exposes
/// (capture date when available, otherwise upload date).
pub fn year_of(date: Option<&str>) -> String {
    let Some(date) = date else {
        return "Sin fecha".into();
    };
    let year = date.get(0..4).unwrap_or("");
    if year.len() == 4
        && year.chars().all(|c| c.is_ascii_digit())
        && year
            .parse::<u32>()
            .is_ok_and(|y| (1900..=2100).contains(&y))
    {
        year.to_string()
    } else {
        "Sin fecha".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> LogicalFile {
        LogicalFile {
            name: name.into(),
            size: 1,
            date: None,
            chunked: false,
            manifest_id: None,
            normal_id: Some(1),
            parts: vec![],
            incomplete: false,
        }
    }

    #[test]
    fn classifies_camera_media() {
        for name in [
            "IMG_0155.WEBP",
            "IMG_20260124_120000.jpg",
            "DSC_0001.JPG",
            "VID_20240101_120000.mp4",
            "XZUD4501.jpg",
            "000001.jpg",
            "2012-05-18 22.11.58.mp4",
            "009746e29731492f99607e97ad7552a2.mov",
        ] {
            assert!(is_camera_capture(&file(name)), "{name}");
        }
    }

    #[test]
    fn rejects_obvious_non_camera_media() {
        for name in [
            "Screenshot 2026-01-01 at 10.00.00.png",
            "WhatsApp Image 2024-01-01 at 10.00.00.jpeg",
            "ScreenRecording_2026.mp4",
            "wp11695996.jpg",
            "[LostYears] episode.mkv_snapshot_01.jpg",
            "Your.Eyes.Only.full.87158.jpg",
            "@instagram_reel.mp4",
            "welcome.png",
        ] {
            assert!(!is_camera_capture(&file(name)), "{name}");
        }
    }

    #[test]
    fn extracts_year() {
        assert_eq!(year_of(Some("2026-08-14T17:11:38Z")), "2026");
        assert_eq!(year_of(Some("2012-05-18T22:11:58Z")), "2012");
        assert_eq!(year_of(None), "Sin fecha");
    }
}
